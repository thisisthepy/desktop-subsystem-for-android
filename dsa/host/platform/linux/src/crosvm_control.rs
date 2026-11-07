//! crosvm's control socket: the commands the host sends a running VM.
//!
//! The host runs the same crosvm program with a command and the VM's socket
//! (`crosvm virtio-mem <bytes> <socket>`, `crosvm suspend <socket>`, ...), which is crosvm's own
//! client for its socket. It resizes the VM through its virtio-mem device, suspends and resumes
//! it (freezing, only when the user chose it), and stops it.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long one command may take.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// A command for a running VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Asks the guest to have this many bytes of the virtio-mem region plugged, above the
    /// memory it booted with.
    VirtioMem { bytes: u64 },
    /// Stops every virtual processor, keeping the VM's memory.
    Suspend,
    /// Lets a suspended VM run again.
    Resume,
    /// Ends the VM at once.
    Stop,
}

/// crosvm's arguments for `request` to the VM at `socket`, after the program name.
pub fn args(request: Request, socket: &Path) -> Vec<String> {
    let socket = socket.to_string_lossy().into_owned();
    match request {
        Request::VirtioMem { bytes } => vec!["virtio-mem".into(), bytes.to_string(), socket],
        Request::Suspend => vec!["suspend".into(), socket],
        Request::Resume => vec!["resume".into(), socket],
        Request::Stop => vec!["stop".into(), socket],
    }
}

/// The control socket of one running VM.
#[derive(Debug, Clone)]
pub struct Control {
    program: PathBuf,
    socket: PathBuf,
}

impl Control {
    pub fn new(program: PathBuf, socket: PathBuf) -> Control {
        Control { program, socket }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Sends `request` and waits for crosvm to answer, at most [`COMMAND_TIMEOUT`].
    pub fn send(&self, request: Request) -> Result<(), String> {
        let mut child = Command::new(&self.program)
            .args(args(request, &self.socket))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run {}: {e}", self.program.display()))?;
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "crosvm did not answer {request:?} within {} s",
                        COMMAND_TIMEOUT.as_secs()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) => return Err(format!("waiting for crosvm: {error}")),
            }
        };
        if status.success() {
            return Ok(());
        }
        let mut why = String::new();
        if let Some(mut err) = child.stderr.take() {
            let _ = err.read_to_string(&mut why);
        }
        Err(format!(
            "crosvm refused {request:?} ({status}): {}",
            why.trim()
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn nfr2_resizing_sets_the_virtio_mem_request_through_the_control_socket() {
        let socket = Path::new("/run/user/1000/dsa/crosvm.sock");
        assert_eq!(
            args(Request::VirtioMem { bytes: 1 << 30 }, socket),
            ["virtio-mem", "1073741824", "/run/user/1000/dsa/crosvm.sock"]
        );
        assert_eq!(args(Request::Suspend, socket)[0], "suspend");
        assert_eq!(args(Request::Resume, socket)[0], "resume");
        assert_eq!(args(Request::Stop, socket)[0], "stop");
    }

    /// A stand-in crosvm that records its arguments, and refuses `suspend`.
    fn fake_crosvm(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join("crosvm");
        let log = dir.join("calls");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\n[ \"$1\" = suspend ] && {{ echo 'no VM' >&2; exit 1; }}\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        program
    }

    #[test]
    fn fr10_control_commands_reach_crosvm_and_its_refusals_are_reported() {
        let dir = std::env::temp_dir().join(format!("dsa-crosvm-control-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let control = Control::new(fake_crosvm(&dir), dir.join("vm.sock"));
        control.send(Request::VirtioMem { bytes: 4096 }).unwrap();
        control.send(Request::Resume).unwrap();
        let refused = control.send(Request::Suspend).unwrap_err();
        assert!(refused.contains("no VM"), "{refused}");
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        let sock = dir.join("vm.sock");
        assert_eq!(
            calls,
            format!(
                "virtio-mem 4096 {s}\nresume {s}\nsuspend {s}\n",
                s = sock.display()
            )
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
