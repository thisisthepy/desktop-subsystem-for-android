//! An unprivileged client for the guest's elevation helper.

#[cfg(any(target_os = "android", target_os = "linux"))]
fn main() {
    let status = match client() {
        Ok(status) => status,
        Err(why) => {
            eprintln!("su: {why}");
            1
        }
    };
    std::process::exit(status);
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn main() {
    eprintln!("su: this client runs only in the Android guest");
    std::process::exit(1);
}

/// How long su waits for its standard input to end when it is not a terminal. An app that
/// hands su its commands and closes the input (`echo id | su`, or a process that writes and
/// closes right after it starts su) has done so by then, and the approval names those
/// commands. An app that keeps the input open to write more later (a shell kept for several
/// commands) is asked for as a shell, since what it will run is not known yet.
#[cfg(any(target_os = "android", target_os = "linux"))]
const STDIN_COMMAND_WAIT_MS: i32 = 250;

/// The most of standard input su reads before it asks for approval.
#[cfg(any(target_os = "android", target_os = "linux"))]
const STDIN_COMMAND_MAX: usize = 4096;

/// Reads standard input (`fd`) until it ends, `timeout_ms` passes or [`STDIN_COMMAND_MAX`]
/// bytes have come. Returns the bytes and whether the input ended.
#[cfg(any(target_os = "android", target_os = "linux"))]
fn read_initial_stdin(fd: i32, timeout_ms: i32) -> (Vec<u8>, bool) {
    let mut collected = Vec::new();
    let mut eof = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);
    while collected.len() < STDIN_COMMAND_MAX {
        let wait = deadline
            .saturating_duration_since(std::time::Instant::now())
            .as_millis() as i32;
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pfd is one valid pollfd on the stack.
        let ready = unsafe { libc::poll(&mut pfd, 1, wait) };
        if ready <= 0 {
            break;
        }
        let mut chunk = [0u8; 1024];
        let max_read = chunk.len().min(STDIN_COMMAND_MAX - collected.len());
        // SAFETY: chunk is a valid mutable byte slice of length >= max_read.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), max_read) };
        if n == 0 {
            eof = true;
            break;
        }
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        collected.extend_from_slice(&chunk[..n as usize]);
    }
    (collected, eof)
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn client() -> Result<i32, Box<dyn std::error::Error>> {
    use dsa_guest_channeld::su_args::{parse, stdin_commands, SuCommand, VERSION, VERSION_CODE};
    use dsa_guest_channeld::su_wire::{self, Frame};
    use std::io::{self, IsTerminal, Read, Write};
    use std::os::unix::net::UnixStream;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = parse(&args)?;
    if let SuCommand::Version { code } = command {
        if code {
            println!("{VERSION_CODE}");
        } else {
            println!("{VERSION}");
        }
        return Ok(0);
    }
    let interactive = io::stdin().is_terminal();
    let (initial_stdin, stdin_eof) = if !interactive && matches!(command, SuCommand::Shell) {
        read_initial_stdin(0, STDIN_COMMAND_WAIT_MS)
    } else {
        (Vec::new(), false)
    };
    // SAFETY: zero is a valid initial winsize and ioctl writes the structure.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    if interactive {
        unsafe { libc::ioctl(0, libc::TIOCGWINSZ as _, &mut size) };
    }
    // The start frame: whether su has a terminal, what it runs (0 a shell, 1 a command, 2 the
    // whole of a standard input that has already ended), the terminal's size, and the text.
    // Commands handed over on standard input are sent whole, so the approval names them and
    // the helper runs exactly what was approved; nothing more is read from standard input.
    let script = std::str::from_utf8(&initial_stdin)
        .ok()
        .filter(|text| stdin_eof && !stdin_commands(text).is_empty())
        .map(str::to_owned);
    let (kind, text) = match (command, script) {
        (SuCommand::Command(command), _) => (1u8, command),
        (_, Some(script)) => (2, script),
        _ => (0, String::new()),
    };
    let preset = kind == 2;
    let mut start = vec![u8::from(interactive), kind];
    start.extend_from_slice(&size.ws_row.to_be_bytes());
    start.extend_from_slice(&size.ws_col.to_be_bytes());
    start.extend_from_slice(text.as_bytes());
    let mut socket = UnixStream::connect("/dev/socket/dsa_su")?;
    su_wire::write_frame(
        &mut socket,
        &Frame {
            kind: su_wire::START,
            payload: start,
        },
    )?;
    let _terminal = if interactive {
        Some(Terminal::raw()?)
    } else {
        None
    };
    let mut input_socket = socket.try_clone()?;
    std::thread::spawn(move || {
        if !preset
            && !initial_stdin.is_empty()
            && su_wire::write_frame(
                &mut input_socket,
                &Frame {
                    kind: su_wire::STDIN,
                    payload: initial_stdin,
                },
            )
            .is_err()
        {
            return;
        }
        if stdin_eof {
            let _ = su_wire::write_frame(
                &mut input_socket,
                &Frame {
                    kind: su_wire::STDIN_EOF,
                    payload: Vec::new(),
                },
            );
            return;
        }
        let mut input = io::stdin().lock();
        let mut bytes = [0; 8192];
        loop {
            match input.read(&mut bytes) {
                Ok(0) => {
                    let _ = su_wire::write_frame(
                        &mut input_socket,
                        &Frame {
                            kind: su_wire::STDIN_EOF,
                            payload: Vec::new(),
                        },
                    );
                    break;
                }
                Ok(n) => {
                    if su_wire::write_frame(
                        &mut input_socket,
                        &Frame {
                            kind: su_wire::STDIN,
                            payload: bytes[..n].to_vec(),
                        },
                    )
                    .is_err()
                    {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
    loop {
        let frame = su_wire::read_frame(&mut socket)?;
        match frame.kind {
            su_wire::STDOUT => io::stdout().lock().write_all(&frame.payload)?,
            su_wire::STDERR => io::stderr().lock().write_all(&frame.payload)?,
            su_wire::EXIT if frame.payload.len() == 4 => {
                let status = i32::from_be_bytes(frame.payload[..].try_into()?);
                return Ok(status.clamp(0, 255));
            }
            _ => return Err("the elevation helper sent an invalid response".into()),
        }
    }
}

#[cfg(any(target_os = "android", target_os = "linux"))]
struct Terminal(libc::termios);

#[cfg(any(target_os = "android", target_os = "linux"))]
impl Terminal {
    fn raw() -> std::io::Result<Self> {
        // SAFETY: tcgetattr writes one initialized termios structure.
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(0, &mut original) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut raw = original;
        // SAFETY: raw points to a valid termios, and fd 0 is the terminal.
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(original))
    }
}

#[cfg(any(target_os = "android", target_os = "linux"))]
impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: the saved attributes came from this terminal.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &self.0) };
    }
}
