//! How the app reaches the DSA service: through the same client the `dsa` command is.
//!
//! A request is the `dsa` command line the other settings apps run, such as
//! `settings set render-gpu integrated --json`. [`LocalApi`] parses it with that client and
//! runs it in this process, over the service's Unix socket (`/run/dsa/host.sock`, or
//! `DSA_SOCKET`). What comes back is the JSON `dsa` prints, which [`crate::model`] reads.
//!
//! Two kinds of request run the `dsa` program instead: the ones that go on until they are
//! stopped (`dsa logcat`), which [`spawn_lines`] reads line by line, and the ones only an
//! administrator may make (machine settings), which [`Elevated`] runs through `pkexec`.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;

/// What kind of thing went wrong, so a page can say it in the user's language. The detail is
/// what the client said, in English, for a bug report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// The `dsa` program could not be started.
    CliMissing,
    /// The DSA service could not be reached.
    ServiceUnreachable,
    /// The service refused or failed the request; the detail says why.
    Refused,
    /// The answer was not the JSON expected.
    Unreadable,
    /// An administrator's permission was asked for and not given.
    NotAuthorized,
}

/// A request that did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub problem: Problem,
    pub detail: String,
}

impl Failure {
    pub fn new(problem: Problem, detail: impl Into<String>) -> Failure {
        Failure {
            problem,
            detail: detail.into(),
        }
    }

    /// Reads what the client said when it failed, as `dsa` prints it on its error output.
    pub fn from_client(error: &str) -> Failure {
        let detail = error.trim();
        let detail = detail.strip_prefix("dsa: ").unwrap_or(detail);
        let problem = if detail.contains("cannot reach the DSA service") {
            Problem::ServiceUnreachable
        } else {
            Problem::Refused
        };
        Failure::new(problem, detail)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

/// Runs one `dsa` request and gives what it printed.
pub trait Dsa: Send + Sync {
    fn run(&self, args: &[String]) -> Result<String, Failure>;
}

/// The local API of the service, reached in this process through the `dsa` client.
pub struct LocalApi;

impl Dsa for LocalApi {
    fn run(&self, args: &[String]) -> Result<String, Failure> {
        let invocation = dsa_cli::parse(args).map_err(|e| Failure::new(Problem::Refused, e))?;
        dsa_cli::connector(invocation.endpoint.as_deref())
            .and_then(|mut connector| dsa_cli::run(&invocation, &mut *connector))
            .map_err(|e| Failure::from_client(&e))
    }
}

/// Where the `dsa` program is: `DSA_CLI` when it is set, then where the packages install
/// it, then the `PATH`.
pub fn dsa_program() -> String {
    if let Ok(path) = std::env::var("DSA_CLI") {
        if !path.is_empty() {
            return path;
        }
    }
    for candidate in ["/usr/lib/dsa/bin/dsa", "/usr/bin/dsa"] {
        if std::path::Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }
    "dsa".to_string()
}

/// Runs `dsa` requests as an administrator: `pkexec` asks the user for permission first.
/// The service lets only an administrator change what applies to every user of the
/// computer, such as the update settings, the traces and the Android version.
pub struct Elevated;

/// pkexec's exit codes for a request the user dismissed or was not allowed.
const PKEXEC_DISMISSED: i32 = 126;
const PKEXEC_NOT_AUTHORIZED: i32 = 127;

impl Dsa for Elevated {
    fn run(&self, args: &[String]) -> Result<String, Failure> {
        let output = Command::new("pkexec")
            .arg(dsa_program())
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| Failure::new(Problem::CliMissing, format!("pkexec: {e}")))?;
        let code = output.status.code().unwrap_or(-1);
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
        if code == PKEXEC_DISMISSED || code == PKEXEC_NOT_AUTHORIZED {
            return Err(Failure::new(
                Problem::NotAuthorized,
                "an administrator's permission was not given",
            ));
        }
        Err(Failure::from_client(&String::from_utf8_lossy(
            &output.stderr,
        )))
    }
}

/// Whether this app runs as the administrator (root), which the service lets change every
/// setting without asking.
pub fn is_administrator() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: no preconditions.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Runs `dsa` with `args` and sends each line it prints to `lines` as it comes, from a
/// thread of its own, until the program ends or the returned child is killed. What it said
/// on its error output, if anything, comes last as an `Err`.
pub fn spawn_lines(
    args: &[String],
    lines: Sender<Result<String, Failure>>,
) -> Result<Child, Failure> {
    let mut child = Command::new(dsa_program())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Failure::new(Problem::CliMissing, format!("{}: {e}", dsa_program())))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    std::thread::spawn(move || {
        if let Some(stdout) = stdout {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if lines.send(Ok(line)).is_err() {
                    return;
                }
            }
        }
        let mut error = String::new();
        if let Some(mut stderr) = stderr {
            use std::io::Read;
            let _ = stderr.read_to_string(&mut error);
        }
        if !error.trim().is_empty() {
            let _ = lines.send(Err(Failure::from_client(&error)));
        }
    });
    Ok(child)
}
