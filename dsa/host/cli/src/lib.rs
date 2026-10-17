//! `dsa`, the command line client of the local API.
//!
//! ```text
//! dsa status
//! dsa start [--wait]
//! dsa stop [--wait]
//! ```
//!
//! It acts on the Android of the user who runs it: the service learns that user from the
//! operating system, so there is no option to name another one.
//!
//! The service is found at the platform's usual place: the named pipe `\\.\pipe\dsa-host`
//! on Windows, `/run/dsa/host.sock` on Linux, and
//! `~/Library/Application Support/DSA/host.sock` on macOS. `--pipe <name>` (Windows) and
//! `--socket <path>` (Linux, macOS) point elsewhere, as do the `DSA_PIPE` and `DSA_SOCKET`
//! environment variables.

use std::fmt::Write as _;
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::host_api::{ClientError, HostApiClient, Status};
use dsa_host_core::session::SessionState;
use dsa_transport::Connector;

pub const USAGE: &str = "usage: dsa <status | start | stop> [--wait] [--timeout <seconds>] \
                         [--pipe <name> | --socket <path>]";

/// What to ask the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Status,
    Start,
    Stop,
}

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub command: Command,
    /// Wait until the Android has started or stopped, instead of returning at once.
    pub wait: bool,
    pub timeout: Duration,
    /// Where the service is, when not at the usual place.
    pub endpoint: Option<String>,
}

/// Parses the arguments after the program name.
pub fn parse(args: &[String]) -> Result<Invocation, String> {
    let mut command = None;
    let mut wait = false;
    let mut timeout = Duration::from_secs(120);
    let mut endpoint = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "status" | "start" | "stop" if command.is_none() => {
                command = Some(match arg.as_str() {
                    "status" => Command::Status,
                    "start" => Command::Start,
                    _ => Command::Stop,
                });
            }
            "--wait" => wait = true,
            "--timeout" => {
                let value = args.next().ok_or("--timeout needs a number of seconds")?;
                let seconds: u64 = value
                    .parse()
                    .map_err(|_| format!("--timeout needs a number of seconds, not {value:?}"))?;
                timeout = Duration::from_secs(seconds);
            }
            "--pipe" | "--socket" => {
                let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
                endpoint = Some(value.clone());
            }
            other => return Err(format!("unexpected {other:?}\n{USAGE}")),
        }
    }
    Ok(Invocation {
        command: command.ok_or(USAGE)?,
        wait,
        timeout,
        endpoint,
    })
}

/// A line describing `status`, and a second one when there is news about an update.
pub fn describe(status: &Status) -> String {
    let mut line = format!("Android: {}", status.state);
    if let Some(reason) = &status.last_reason {
        let _ = write!(line, " (last stopped: {reason})");
    }
    if let Some(notice) = &status.update_notice {
        let _ = write!(line, "\nUpdate: {notice}");
    }
    line
}

/// Runs `invocation` against the service `connector` reaches. Returns what to print.
pub fn run(invocation: &Invocation, connector: &mut dyn Connector) -> Result<String, String> {
    let build = format!("dsa {}", env!("CARGO_PKG_VERSION"));
    let mut client =
        HostApiClient::connect(connector, &build, Duration::from_secs(10)).map_err(|error| {
            match error {
                ClientError::Connect(_) => format!(
                "cannot reach the DSA service ({error}); check that it is installed and running"
            ),
                other => other.to_string(),
            }
        })?;
    let status = match invocation.command {
        Command::Status => client.status(),
        Command::Start => client.start(),
        Command::Stop => client.stop(),
    }
    .map_err(|e| e.to_string())?;
    if !invocation.wait || invocation.command == Command::Status {
        return Ok(describe(&status));
    }
    let deadline = Instant::now() + invocation.timeout;
    // The service answers a start once the session has moved on, so a Stopped answer
    // means the start has already failed.
    let (done, failed): (&[SessionState], &[SessionState]) = match invocation.command {
        Command::Start => (
            &[SessionState::Active, SessionState::Dozing],
            &[SessionState::Stopped],
        ),
        _ => (&[SessionState::Stopped], &[]),
    };
    let goal = if invocation.command == Command::Start {
        "started"
    } else {
        "stopped"
    };
    let mut status = status;
    loop {
        if done.contains(&status.state) {
            return Ok(describe(&status));
        }
        if failed.contains(&status.state) {
            return Err(format!("Android did not start: {}", describe(&status)));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Android has not {goal} after {} seconds: {}",
                invocation.timeout.as_secs(),
                describe(&status)
            ));
        }
        thread::sleep(Duration::from_millis(250));
        status = client.status().map_err(|e| e.to_string())?;
    }
}

/// The connector for the platform's usual place, or for `endpoint`.
pub fn connector(endpoint: Option<&str>) -> Result<Box<dyn Connector>, String> {
    #[cfg(windows)]
    {
        let name = endpoint
            .map(str::to_string)
            .or_else(|| std::env::var("DSA_PIPE").ok())
            .unwrap_or_else(|| "dsa-host".into());
        Ok(Box::new(dsa_host_windows::pipe::NamedPipeConnector {
            name,
        }))
    }
    #[cfg(unix)]
    {
        let path = match endpoint
            .map(str::to_string)
            .or_else(|| std::env::var("DSA_SOCKET").ok())
        {
            Some(path) => std::path::PathBuf::from(path),
            None => default_socket()?,
        };
        Ok(Box::new(dsa_host_unix::socket::UnixLocalConnector { path }))
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = endpoint;
        Err("this platform has no DSA service".into())
    }
}

#[cfg(unix)]
fn default_socket() -> Result<std::path::PathBuf, String> {
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
        Ok(std::path::PathBuf::from(home).join("Library/Application Support/DSA/host.sock"))
    } else {
        Ok("/run/dsa/host.sock".into())
    }
}
