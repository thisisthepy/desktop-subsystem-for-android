//! Android's shared storage at one address that does not change: the files door.
//!
//! A session serves its guest's shared storage on a loopback port of its own, chosen when the
//! session first shows it ([`crate::session::Session::android_files`]). That address is gone
//! once the service restarts, and it does not exist while Android is stopped, so nothing the
//! user keeps (a place in the file manager's side bar) can point at it. The door is the
//! address that stays: one loopback port, [`DOOR_PORT`], opened by the service when it starts.
//!
//! Every user has a key of their own, a long random name only they (and the service) can read.
//! The first segment of every path through the door is that key. The door asks the platform
//! whose key it is ([`KeyOwners`]), has that user's Android serve its storage under the key
//! itself as the token ([`crate::host_api::UserAndroid::android_files_with_token`]), starting
//! Android when it is stopped, and from then on copies the connection's bytes both ways
//! unchanged. A path with a key nobody has is refused, and so is a request for anything but
//! the door's own empty root, which a WebDAV client looks at before the folder under it.
//!
//! Nothing here names a host operating system API.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::adb_bridge::{splice, BridgeStream};
use crate::host_api::UserAndroids;

/// The loopback port of the door.
pub const DOOR_PORT: u16 = 47591;

/// The longest request head the door reads.
const MAX_HEAD: usize = 64 * 1024;

/// How long the door waits for a stopped Android to start and serve its files.
pub const START_WAIT: Duration = Duration::from_secs(120);

/// The user a key belongs to, as the service names users; `None` for a key nobody has.
pub type KeyOwners = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// A running door. Dropping it stops taking connections.
pub struct FilesDoor {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    accepting: Option<JoinHandle<()>>,
}

impl FilesDoor {
    /// Opens the door on `127.0.0.1:port` (0 takes any free port, for tests). It fails when
    /// the port is taken.
    pub fn start(
        port: u16,
        owners: KeyOwners,
        androids: Arc<dyn UserAndroids>,
        wait: Duration,
    ) -> io::Result<FilesDoor> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let accepting = {
            let stop = Arc::clone(&stop);
            thread::Builder::new()
                .name("dsa-files-door".into())
                .spawn(move || {
                    for connection in listener.incoming() {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }
                        let Ok(connection) = connection else {
                            continue;
                        };
                        let (owners, androids) = (Arc::clone(&owners), Arc::clone(&androids));
                        let _ = thread::Builder::new()
                            .name("dsa-files-door-connection".into())
                            .spawn(move || {
                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        if let Err(why) =
                                            serve(connection, &owners, &*androids, wait)
                                        {
                                            eprintln!("files door: {why}");
                                        }
                                    }));
                            });
                    }
                })?
        };
        Ok(FilesDoor {
            address,
            stop,
            accepting: Some(accepting),
        })
    }

    /// The address the door listens on.
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for FilesDoor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the accepting thread so it sees the stop.
        let _ = TcpStream::connect(self.address);
        if let Some(accepting) = self.accepting.take() {
            let _ = accepting.join();
        }
    }
}

/// One request head, as read: its bytes, its method, its path and its body's length.
struct Head {
    bytes: Vec<u8>,
    method: String,
    path: String,
    body: u64,
}

/// Reads one request head; `None` when the client closed the connection between requests.
fn read_head(stream: &mut impl BufRead) -> io::Result<Option<Head>> {
    let mut bytes = Vec::new();
    loop {
        let mut line = Vec::new();
        if stream.read_until(b'\n', &mut line)? == 0 {
            if bytes.is_empty() {
                return Ok(None);
            }
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if bytes.is_empty() && (line == b"\r\n" || line == b"\n") {
            continue;
        }
        bytes.extend_from_slice(&line);
        if bytes.len() > MAX_HEAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request head too long",
            ));
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let mut lines = text.lines();
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default().to_ascii_uppercase();
    let target = first.next().unwrap_or_default();
    let path = target
        .strip_prefix("http://")
        .and_then(|rest| rest.find('/').map(|at| &rest[at..]))
        .unwrap_or(target);
    let path = path.split('?').next().unwrap_or_default().to_string();
    let mut body = 0;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                body = value.trim().parse().unwrap_or(0);
            }
            if name.trim().eq_ignore_ascii_case("Transfer-Encoding") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a chunked request before the folder is chosen",
                ));
            }
        }
    }
    Ok(Some(Head {
        bytes,
        method,
        path,
        body,
    }))
}

/// The key a request path starts with; `None` for the door's root.
pub fn key_of(path: &str) -> Option<&str> {
    path.split('/').find(|segment| !segment.is_empty())
}

/// Whether `key` could be a key at all: what the guest takes as a token.
fn plain_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 64 && key.chars().all(|c| c.is_ascii_alphanumeric())
}

fn answer(out: &mut impl Write, status: &str, extra: &str, body: &str) -> io::Result<()> {
    write!(
        out,
        "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nDAV: 1, 2\r\nMS-Author-Via: DAV\r\n\r\n{body}",
        body.len()
    )?;
    out.flush()
}

/// The door's root, as a WebDAV client sees it: an empty folder that names nobody's key.
const ROOT: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?><D:multistatus xmlns:D=\"DAV:\">\
    <D:response><D:href>/</D:href><D:propstat><D:prop><D:displayname/>\
    <D:resourcetype><D:collection/></D:resourcetype></D:prop>\
    <D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>";

const ALLOW: &str =
    "Allow: OPTIONS, PROPFIND, PROPPATCH, GET, HEAD, PUT, DELETE, MKCOL, COPY, MOVE, LOCK, UNLOCK\r\n";

/// Serves one connection: the door's own answers until a request names a key, then the
/// owner's Android, to the end of the connection.
fn serve(
    connection: TcpStream,
    owners: &KeyOwners,
    androids: &dyn UserAndroids,
    wait: Duration,
) -> Result<(), String> {
    let mut out = connection.try_clone().map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(connection);
    loop {
        let Some(head) = read_head(&mut reader).map_err(|e| e.to_string())? else {
            return Ok(());
        };
        let mut skip = (&mut reader).take(head.body);
        let key = key_of(&head.path).filter(|key| plain_key(key));
        // OPTIONS says what the server does, wherever it is asked; it needs no Android.
        if head.method == "OPTIONS" {
            io::copy(&mut skip, &mut io::sink()).map_err(|e| e.to_string())?;
            answer(&mut out, "200 OK", ALLOW, "").map_err(|e| e.to_string())?;
            continue;
        }
        let Some(key) = key else {
            io::copy(&mut skip, &mut io::sink()).map_err(|e| e.to_string())?;
            let wrote = if key_of(&head.path).is_none() && head.method == "PROPFIND" {
                answer(
                    &mut out,
                    "207 Multi-Status",
                    "Content-Type: application/xml; charset=utf-8\r\n",
                    ROOT,
                )
            } else {
                answer(&mut out, "403 Forbidden", "", "")
            };
            wrote.map_err(|e| e.to_string())?;
            continue;
        };
        let Some(user) = owners(key) else {
            io::copy(&mut skip, &mut io::sink()).map_err(|e| e.to_string())?;
            answer(&mut out, "403 Forbidden", "", "").map_err(|e| e.to_string())?;
            continue;
        };
        let upstream = match reach(androids, &user, key, wait) {
            Ok(upstream) => upstream,
            Err(why) => {
                io::copy(&mut skip, &mut io::sink()).map_err(|e| e.to_string())?;
                let _ = answer(&mut out, "503 Service Unavailable", "", "");
                return Err(format!("Android's files for {user}: {why}"));
            }
        };
        // The head goes first, then whatever the client already sent after it, then the rest
        // of the connection, both ways.
        let mut up = upstream.try_clone().map_err(|e| e.to_string())?;
        up.write_all(&head.bytes).map_err(|e| e.to_string())?;
        up.write_all(reader.buffer()).map_err(|e| e.to_string())?;
        let client = reader.into_inner();
        return splice(
            Box::new(client) as Box<dyn BridgeStream>,
            Box::new(upstream) as Box<dyn BridgeStream>,
        )
        .map_err(|e| e.to_string());
    }
}

/// A connection to `user`'s Android's files, served under `key`, once they answer: Android
/// is started when it is stopped, and is waited for at most `wait`.
fn reach(
    androids: &dyn UserAndroids,
    user: &str,
    key: &str,
    wait: Duration,
) -> Result<TcpStream, String> {
    let android = androids.android(user)?;
    let deadline = Instant::now() + wait;
    let mut pause = FIRST_PAUSE;
    let mut said: Option<String> = None;
    loop {
        let last = match android
            .android_files_with_token(key)
            .and_then(|url| address_of(&url))
        {
            Ok(address) => match probe(address, key) {
                Ok(stream) => return Ok(stream),
                Err(why) => why,
            },
            Err(why) => why,
        };
        let now = Instant::now();
        if now >= deadline {
            return Err(last);
        }
        // A refusal that keeps coming is said once, not on every try.
        if said.as_deref() != Some(last.as_str()) {
            eprintln!("files door: Android's files for {user} are not there yet: {last}");
            said = Some(last);
        }
        thread::sleep(pause.min(deadline - now));
        pause = next_pause(pause);
    }
}

/// How long the door waits before it asks the guest again the first time.
pub const FIRST_PAUSE: Duration = Duration::from_millis(500);

/// The longest the door waits between two tries.
pub const LONGEST_PAUSE: Duration = Duration::from_secs(8);

/// The wait after `pause`: twice as long, up to [`LONGEST_PAUSE`]. A server that refuses
/// every request (its sandbox denying the storage) is then asked a few times a minute, not
/// twice a second.
pub fn next_pause(pause: Duration) -> Duration {
    (pause * 2).min(LONGEST_PAUSE)
}

/// The socket address of an `http://127.0.0.1:<port>/...` URL.
fn address_of(url: &str) -> Result<SocketAddr, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("{url} is not an http address"))?;
    let authority = rest.split('/').next().unwrap_or_default();
    authority
        .parse()
        .map_err(|_| format!("{url} has no address"))
}

/// Asks the guest's server for the folder under `key` and, when it is there, hands back a
/// fresh connection for the client. Right after the token changes the guest's old server can
/// still be answering, with a refusal; the answer is only taken once it is the folder.
fn probe(address: SocketAddr, key: &str) -> Result<TcpStream, String> {
    let mut stream =
        TcpStream::connect_timeout(&address, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    write!(
        stream,
        "PROPFIND /{key}/ HTTP/1.1\r\nHost: {address}\r\nDepth: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| e.to_string())?;
    let mut status = String::new();
    BufReader::new(&stream)
        .read_line(&mut status)
        .map_err(|e| e.to_string())?;
    if !status.starts_with("HTTP/1.1 207") {
        return Err(if status.is_empty() {
            "Android's file server is not up yet".to_string()
        } else {
            format!("Android's file server answered {}", status.trim())
        });
    }
    let fresh =
        TcpStream::connect_timeout(&address, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    Ok(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr29_a_refusing_guest_is_asked_again_less_and_less_often() {
        let mut pause = FIRST_PAUSE;
        let mut tries_in_a_minute = 0;
        let mut spent = Duration::ZERO;
        while spent < Duration::from_secs(60) {
            spent += pause;
            tries_in_a_minute += 1;
            pause = next_pause(pause);
        }
        assert_eq!(next_pause(FIRST_PAUSE), Duration::from_secs(1));
        assert_eq!(next_pause(LONGEST_PAUSE), LONGEST_PAUSE);
        assert!(tries_in_a_minute <= 12, "asked {tries_in_a_minute} times");
    }

    #[test]
    fn fr29_the_first_path_segment_is_the_key() {
        assert_eq!(key_of("/abc/Pictures/cat.jpg"), Some("abc"));
        assert_eq!(key_of("/abc"), Some("abc"));
        assert_eq!(key_of("/"), None);
        assert_eq!(key_of(""), None);
        assert!(plain_key("3f9a"));
        assert!(!plain_key("a b"));
        assert!(!plain_key("DavWWWRoot.."));
    }
}
