//! Reading logd: the requests its reader socket takes and the entries it sends, as the
//! channel daemon turns them into the control protocol's [`LogRecord`].
//!
//! logd keeps Android's log in memory. A reader connects to `/dev/socket/logdr` (a
//! `SOCK_SEQPACKET` socket), writes one request line, and gets one entry per packet: a
//! `logger_entry` header, then the payload. Nothing here touches a socket except
//! [`LogdSocket`] (Linux and Android only), so the parsing runs and is tested on any platform.

use std::fmt::Write as _;

use dsa_protocol::control::LogRecord;

/// logd's reader socket.
pub const LOGDR_SOCKET: &str = "/dev/socket/logdr";

/// The largest packet logd sends: `LOGGER_ENTRY_MAX_LEN`.
pub const MAX_ENTRY: usize = 5 * 1024;

/// logd's log IDs with a binary payload: events, stats and security.
const BINARY_IDS: [u32; 3] = [2, 5, 6];

/// The highest log ID logd has (kernel).
const MAX_LOG_ID: u32 = 7;

/// The request line for logd: `stream` follows the log until the socket closes, `dumpAndClose`
/// sends what logd holds and closes. `buffers` has bit (1 << (log ID + 1)) for each buffer,
/// as the control protocol numbers them. `tail` starts with that many of the newest entries;
/// 0 starts with new ones only (from `start_ns`, the guest's wall clock now).
pub fn request(buffers: u32, dump: bool, tail: u32, start_ns: Option<u64>) -> String {
    let ids: Vec<String> = (0..=MAX_LOG_ID)
        .filter(|id| buffers & (1 << (id + 1)) != 0)
        .map(|id| id.to_string())
        .collect();
    let mut line = String::from(if dump { "dumpAndClose" } else { "stream" });
    let _ = write!(line, " lids={}", ids.join(","));
    if tail > 0 {
        let _ = write!(line, " tail={tail}");
    } else if let Some(start) = start_ns {
        let _ = write!(
            line,
            " start={}.{:09}",
            start / 1_000_000_000,
            start % 1_000_000_000
        );
    }
    line
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Text up to the first NUL, invalid UTF-8 replaced.
fn c_text(bytes: &[u8]) -> (String, usize) {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    (
        String::from_utf8_lossy(&bytes[..end]).into_owned(),
        (end + 1).min(bytes.len()),
    )
}

/// One entry as logd sends it. An error says what is wrong with the packet.
pub fn parse_entry(packet: &[u8]) -> Result<LogRecord, String> {
    let len = u16_at(packet, 0).ok_or("a packet shorter than its header")? as usize;
    let header = u16_at(packet, 2).ok_or("a packet shorter than its header")? as usize;
    // logger_entry v4: len, hdr_size, pid, tid, sec, nsec, lid, uid.
    if header < 28 || packet.len() < header {
        return Err(format!("a packet with a {header}-byte header"));
    }
    let payload = packet
        .get(header..header + len)
        .ok_or_else(|| format!("a packet whose payload of {len} bytes is cut short"))?;
    let pid = u32_at(packet, 4).unwrap_or(0) as i32;
    let tid = u32_at(packet, 8).unwrap_or(0) as i32;
    let sec = u32_at(packet, 12).unwrap_or(0);
    let nsec = u32_at(packet, 16).unwrap_or(0);
    let lid = u32_at(packet, 20).unwrap_or(0);
    let uid = u32_at(packet, 24).unwrap_or(0);
    if lid > MAX_LOG_ID {
        return Err(format!("a packet from log {lid}, which logd has not"));
    }
    let (priority, tag, message) = if BINARY_IDS.contains(&lid) {
        let tag = u32_at(payload, 0).ok_or("a binary entry without its tag")?;
        (4, tag.to_string(), event_text(payload.get(4..).unwrap_or_default()))
    } else {
        let priority = *payload.first().ok_or("an entry without a priority")?;
        let (tag, used) = c_text(payload.get(1..).unwrap_or_default());
        let (message, _) = c_text(payload.get(1 + used..).unwrap_or_default());
        (u32::from(priority.min(8)), tag, message)
    };
    Ok(LogRecord {
        buffer: (lid + 1) as i32,
        priority,
        time_ns: u64::from(sec) * 1_000_000_000 + u64::from(nsec.min(999_999_999)),
        pid,
        tid,
        uid,
        tag,
        message: message.trim_end_matches('\n').to_string(),
        dropped: 0,
    })
}

/// A binary log payload as text: integers, longs, floats, strings and lists, as logcat shows
/// an event whose tag it cannot name.
pub fn event_text(payload: &[u8]) -> String {
    let mut out = String::new();
    let mut at = 0;
    while at < payload.len() {
        if !event_value(payload, &mut at, &mut out, 0) {
            break;
        }
        if at < payload.len() {
            out.push(' ');
        }
    }
    out
}

fn event_value(payload: &[u8], at: &mut usize, out: &mut String, depth: u8) -> bool {
    let Some(&kind) = payload.get(*at) else {
        return false;
    };
    *at += 1;
    match kind {
        0 => match u32_at(payload, *at) {
            Some(v) => {
                let _ = write!(out, "{}", v as i32);
                *at += 4;
                true
            }
            None => false,
        },
        1 => match payload.get(*at..*at + 8) {
            Some(bytes) => {
                let v = i64::from_le_bytes(bytes.try_into().unwrap_or([0; 8]));
                let _ = write!(out, "{v}");
                *at += 8;
                true
            }
            None => false,
        },
        2 => {
            let Some(len) = u32_at(payload, *at) else {
                return false;
            };
            *at += 4;
            let end = (*at + len as usize).min(payload.len());
            out.push_str(&String::from_utf8_lossy(&payload[*at..end]));
            *at = end;
            true
        }
        3 if depth < 8 => {
            let Some(&count) = payload.get(*at) else {
                return false;
            };
            *at += 1;
            out.push('[');
            for i in 0..count {
                if i > 0 {
                    out.push(',');
                }
                if !event_value(payload, at, out, depth + 1) {
                    out.push(']');
                    return false;
                }
            }
            out.push(']');
            true
        }
        4 => match u32_at(payload, *at) {
            Some(bits) => {
                let _ = write!(out, "{}", f32::from_bits(bits));
                *at += 4;
                true
            }
            None => false,
        },
        _ => false,
    }
}

/// logcat's letter for a priority.
fn letter(priority: u32) -> char {
    match priority {
        3 => 'D',
        4 => 'I',
        5 => 'W',
        6 => 'E',
        7 => 'F',
        8 => 'S',
        _ => 'V',
    }
}

/// A record as logcat's `threadtime` prints it, with the year and in UTC, for the text the
/// diagnostics carry.
pub fn threadtime(record: &LogRecord) -> String {
    if record.dropped > 0 {
        return format!("--------- {} records dropped\n", record.dropped);
    }
    let seconds = record.time_ns / 1_000_000_000;
    let millis = (record.time_ns / 1_000_000) % 1000;
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let stamp = format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{millis:03}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    );
    let mut out = String::new();
    for line in record.message.split('\n') {
        let _ = writeln!(
            out,
            "{stamp} {:5} {:5} {} {}: {line}",
            record.pid,
            record.tid,
            letter(record.priority),
            record.tag
        );
    }
    out
}

/// The guest's wall clock, in nanoseconds since the Unix epoch.
pub fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// A connection to logd's reader socket.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub struct LogdSocket(std::os::fd::OwnedFd);

#[cfg(any(target_os = "linux", target_os = "android"))]
impl LogdSocket {
    /// Connects to logd at `path` and sends `request`.
    pub fn open(path: &str, request: &str) -> std::io::Result<LogdSocket> {
        use std::io::{Error, ErrorKind};
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        // SAFETY: plain socket creation; the result is checked.
        let fd = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
            )
        };
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        // SAFETY: fd is a new descriptor this function owns from here on.
        let socket = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: sockaddr_un is plain data; zero is a valid starting value.
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = path.as_bytes();
        if bytes.len() >= address.sun_path.len() {
            return Err(Error::new(ErrorKind::InvalidInput, "the socket path is too long"));
        }
        for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
            *slot = *byte as libc::c_char;
        }
        // SAFETY: address is a valid sockaddr_un of the size given.
        let connected = unsafe {
            libc::connect(
                socket.as_raw_fd(),
                &address as *const libc::sockaddr_un as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        if connected < 0 {
            return Err(Error::last_os_error());
        }
        // SAFETY: writes from a valid buffer of the length given.
        let written = unsafe {
            libc::write(
                socket.as_raw_fd(),
                request.as_ptr() as *const libc::c_void,
                request.len(),
            )
        };
        if written < 0 {
            return Err(Error::last_os_error());
        }
        Ok(LogdSocket(socket))
    }

    /// The next packet, waiting at most `timeout_ms`. `Ok(None)` when none came in time; an
    /// empty packet when logd closed the socket (a dump is over).
    pub fn next(&mut self, timeout_ms: i32) -> std::io::Result<Option<Vec<u8>>> {
        use std::os::fd::AsRawFd;
        let mut poll = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut poll, 1, timeout_ms) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(error);
        }
        if ready == 0 {
            return Ok(None);
        }
        let mut packet = vec![0u8; MAX_ENTRY + 64];
        // SAFETY: reads into a buffer of the length given.
        let read = unsafe {
            libc::read(
                self.0.as_raw_fd(),
                packet.as_mut_ptr() as *mut libc::c_void,
                packet.len(),
            )
        };
        if read < 0 {
            return Err(std::io::Error::last_os_error());
        }
        packet.truncate(read as usize);
        Ok(Some(packet))
    }
}
