//! Run an approved shell and carry its streams back to the su client.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use crate::su_wire::{self, Frame};

fn terminal() -> io::Result<(File, File)> {
    // SAFETY: posix_openpt takes no pointers and returns a fresh descriptor.
    let fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd was returned by posix_openpt and is uniquely owned.
    let master = unsafe { File::from_raw_fd(fd) };
    // SAFETY: fd is an open PTY master.
    if unsafe { libc::grantpt(fd) } != 0 || unsafe { libc::unlockpt(fd) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut path: [libc::c_char; 256] = [0; 256];
    // SAFETY: path has room for the name and fd is an open PTY master.
    if unsafe { libc::ptsname_r(fd, path.as_mut_ptr(), path.len()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ptsname_r returned a NUL-terminated name.
    let path = unsafe { std::ffi::CStr::from_ptr(path.as_ptr()) }
        .to_str()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid PTY name"))?;
    let slave = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(path)?;
    Ok((master, slave))
}

fn send(socket: &Mutex<UnixStream>, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut socket = socket.lock().unwrap_or_else(|e| e.into_inner());
    su_wire::write_frame(
        &mut *socket,
        &Frame {
            kind,
            payload: payload.to_vec(),
        },
    )
}

fn pump_output(
    mut source: impl Read + Send + 'static,
    socket: Arc<Mutex<UnixStream>>,
    kind: u8,
    done: mpsc::Sender<()>,
) {
    std::thread::spawn(move || {
        let mut buf = [0; 8192];
        loop {
            match source.read(&mut buf) {
                Ok(0) => break,
                Ok(n) if send(&socket, kind, &buf[..n]).is_ok() => {}
                Ok(_) => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = done.send(());
    });
}

/// Whether the child `pid` has ended, leaving it unreaped so its process ID (and the process
/// group it leads) cannot be taken by another process yet.
fn exited(pid: u32) -> io::Result<bool> {
    // SAFETY: zero is a valid siginfo_t, and waitid writes at most one into it.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: info points to a valid siginfo_t.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            // The kernel leaves si_signo 0 when the child has not ended, and sets SIGCHLD
            // when it has.
            return Ok(info.si_signo == libc::SIGCHLD);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// The shell inherits only streams made by the helper, never descriptors from an app.
pub fn run(
    socket: UnixStream,
    command: Option<String>,
    interactive: bool,
    rows: u16,
    cols: u16,
) -> io::Result<()> {
    run_shell(socket, command, interactive, rows, cols, "/system/bin/sh")
}

fn run_shell(
    socket: UnixStream,
    command: Option<String>,
    interactive: bool,
    rows: u16,
    cols: u16,
    executable: &str,
) -> io::Result<()> {
    // A disconnected caller must not cause a newly approved command to run.
    let mut poll = libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLHUP,
        revents: 0,
    };
    // SAFETY: poll references one valid pollfd.
    if unsafe { libc::poll(&mut poll, 1, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if poll.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "the caller disconnected",
        ));
    }
    // The shell starts from the helper's environment (init's, never the app's), without what
    // only the helper needs (the sockets init passed it) or anything that would load code into
    // a root process, and with root's own names.
    let mut shell = Command::new(executable);
    for (name, _) in std::env::vars_os() {
        if name
            .to_str()
            .is_some_and(|name| name.starts_with("ANDROID_SOCKET_"))
        {
            shell.env_remove(name);
        }
    }
    shell
        .env_remove("LD_PRELOAD")
        .env_remove("LD_LIBRARY_PATH")
        .env("USER", "root")
        .env("LOGNAME", "root")
        .env("HOME", "/data")
        .env("SHELL", executable);
    if std::env::var_os("PATH").is_none_or(|p| p.is_empty()) {
        shell.env(
            "PATH",
            "/product/bin:/apex/com.android.runtime/bin:/apex/com.android.art/bin:/system_ext/bin:/system/bin:/system/xbin:/odm/bin:/vendor/bin:/vendor/xbin",
        );
    }
    if let Some(command) = command {
        shell.arg("-c").arg(command);
    }
    let mut pty_master = None;
    if interactive {
        let (master, slave) = terminal()?;
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: master is a PTY and size points to a valid winsize.
        if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ as _, &size) } < 0 {
            return Err(io::Error::last_os_error());
        }
        shell.stdin(Stdio::from(slave.try_clone()?));
        shell.stdout(Stdio::from(slave.try_clone()?));
        shell.stderr(Stdio::from(slave));
        pty_master = Some(master);
    } else {
        shell
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    }
    // SAFETY: only async-signal-safe system calls run between fork and exec.
    unsafe {
        shell.pre_exec(move || {
            // Root's credentials alone: no supplementary group of the helper's carries over.
            // init starts the helper as root with no other group, so the groups are dropped
            // only when some are there (dropping them needs a capability the helper's domain
            // is not given), and a shell that cannot shed them does not start.
            if libc::geteuid() == 0 {
                let mut groups: [libc::gid_t; 64] = [0; 64];
                let count = libc::getgroups(groups.len() as libc::c_int, groups.as_mut_ptr());
                let others = count < 0 || groups[..count as usize].iter().any(|group| *group != 0);
                let root_gid: libc::gid_t = 0;
                if (others && libc::setgroups(1, &root_gid) < 0)
                    || libc::setresgid(0, 0, 0) < 0
                    || libc::setresuid(0, 0, 0) < 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if interactive && libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // Everything that can fail is made before the shell starts, so no error path between the
    // spawn and the wait below leaves the shell running without anyone waiting for it.
    let output = Arc::new(Mutex::new(socket.try_clone()?));
    let mut read_socket = socket.try_clone()?;
    let pty_reader = pty_master.as_ref().map(File::try_clone).transpose()?;
    let mut child = shell.spawn()?;
    let (done, finished) = mpsc::channel();
    let (input, output_count): (Box<dyn Write + Send>, usize) =
        if let (Some(master), Some(reader)) = (pty_master, pty_reader) {
            pump_output(reader, Arc::clone(&output), su_wire::STDOUT, done);
            (Box::new(master), 1)
        } else {
            pump_output(
                child.stdout.take().unwrap(),
                Arc::clone(&output),
                su_wire::STDOUT,
                done.clone(),
            );
            pump_output(
                child.stderr.take().unwrap(),
                Arc::clone(&output),
                su_wire::STDERR,
                done,
            );
            (Box::new(child.stdin.take().unwrap()), 2)
        };
    let (disconnected, connection) = mpsc::channel();
    std::thread::spawn(move || {
        let mut input = Some(input);
        loop {
            match su_wire::read_frame(&mut read_socket) {
                Ok(frame) if frame.kind == su_wire::STDIN && input.is_some() => {
                    if let Some(writer) = input.as_mut() {
                        if writer.write_all(&frame.payload).is_err() {
                            input = None;
                        }
                    }
                }
                Ok(frame)
                    if frame.kind == su_wire::STDIN_EOF
                        && frame.payload.is_empty()
                        && input.is_some() =>
                {
                    if let (true, Some(writer)) = (interactive, input.as_mut()) {
                        let _ = writer.write_all(&[4]);
                    }
                    input = None;
                }
                _ => {
                    let _ = disconnected.send(());
                    break;
                }
            }
        }
    });
    let status = loop {
        match exited(child.id()) {
            Ok(true) => {
                // Root lasts only as long as the approved command: whatever the shell left
                // running in its session (a command sent to the background, a daemon that did
                // not start a session of its own) ends with it. The shell is not reaped yet, so
                // its process group's ID still names this group and no other.
                // SAFETY: the negative PID addresses the process group created by setsid.
                unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
                break child.wait()?;
            }
            Ok(false) => {}
            Err(error) => {
                // SAFETY: the negative PID addresses the process group created by setsid.
                unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
        if connection.try_recv().is_ok() {
            // The shell created a session of its own. Stop its children as well when the
            // caller goes away, so they do not keep running without a client.
            // SAFETY: the negative PID addresses the process group created by setsid.
            unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    for _ in 0..output_count {
        if finished
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_err()
        {
            break;
        }
    }
    let code = status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1));
    let result = send(&output, su_wire::EXIT, &code.to_be_bytes());
    let _ = socket.shutdown(std::net::Shutdown::Both);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr35_a_shell_carries_standard_input_output_and_exit_code() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            run_shell(server, None, false, 0, 0, "/bin/sh").unwrap();
        });
        su_wire::write_frame(
            &mut client,
            &Frame {
                kind: su_wire::STDIN,
                payload: b"printf out; printf err >&2; exit 7\n".to_vec(),
            },
        )
        .unwrap();
        su_wire::write_frame(
            &mut client,
            &Frame {
                kind: su_wire::STDIN_EOF,
                payload: Vec::new(),
            },
        )
        .unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        loop {
            let frame = su_wire::read_frame(&mut client).unwrap();
            match frame.kind {
                su_wire::STDOUT => out.extend(frame.payload),
                su_wire::STDERR => err.extend(frame.payload),
                su_wire::EXIT => {
                    assert_eq!(frame.payload, 7i32.to_be_bytes());
                    break;
                }
                _ => panic!("unexpected output frame"),
            }
        }
        assert_eq!(out, b"out");
        assert_eq!(err, b"err");
        worker.join().unwrap();
    }

    #[test]
    fn fr35_nothing_the_approved_command_left_running_outlives_it() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            run_shell(
                server,
                Some("sleep 300 & echo $!".into()),
                false,
                0,
                0,
                "/bin/sh",
            )
            .unwrap();
        });
        let mut output = Vec::new();
        loop {
            let frame = su_wire::read_frame(&mut client).unwrap();
            match frame.kind {
                su_wire::STDOUT => output.extend(frame.payload),
                su_wire::EXIT => {
                    assert_eq!(frame.payload, 0i32.to_be_bytes());
                    break;
                }
                _ => {}
            }
        }
        worker.join().unwrap();
        let pid: u32 = String::from_utf8(output).unwrap().trim().parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // Gone, or a zombie waiting for whoever took it over to reap it.
            let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    let (_, rest) = stat.rsplit_once(')')?;
                    rest.trim_start().chars().next()
                });
            if matches!(state, None | Some('Z' | 'X')) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the background command {pid} still runs as {state:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn fr35_an_interactive_shell_has_a_controlling_pty() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            run_shell(
                server,
                Some("test -t 0 && printf tty".into()),
                true,
                24,
                80,
                "/bin/sh",
            )
            .unwrap();
        });
        let mut output = Vec::new();
        loop {
            let frame = su_wire::read_frame(&mut client).unwrap();
            if frame.kind == su_wire::EXIT {
                assert_eq!(frame.payload, 0i32.to_be_bytes());
                break;
            }
            if frame.kind == su_wire::STDOUT {
                output.extend(frame.payload);
            }
        }
        assert_eq!(output, b"tty");
        worker.join().unwrap();
    }
}
