//! The files door: one loopback address for Android's shared storage that holds while
//! Android is stopped and across sessions.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsa_host_core::files_door::{FilesDoor, KeyOwners};
use dsa_host_core::host_api::{PerUser, UserAndroid};
use dsa_host_core::ime::ImeEvent;
use dsa_host_core::input::PointerEvent;
use dsa_host_core::keys::KeyEvent;
use dsa_host_core::session::SessionState;
use dsa_host_core::tasks::TaskRequestError;

/// An Android whose file server answers 207 to a path under the token it was last given, and
/// 403 to anything else, as the guest's does.
struct FilesAndroid {
    state: Mutex<SessionState>,
    token: Arc<Mutex<Option<String>>>,
    server: Mutex<Option<SocketAddr>>,
}

impl FilesAndroid {
    fn new() -> FilesAndroid {
        FilesAndroid {
            state: Mutex::new(SessionState::Stopped),
            token: Arc::default(),
            server: Mutex::new(None),
        }
    }

    fn serve(&self) -> SocketAddr {
        let mut server = self.server.lock().unwrap();
        if let Some(address) = *server {
            return address;
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let token = Arc::clone(&self.token);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let token = Arc::clone(&token);
                std::thread::spawn(move || {
                    let mut out = stream.try_clone().unwrap();
                    let mut reader = BufReader::new(stream);
                    loop {
                        let mut first = String::new();
                        if reader.read_line(&mut first).unwrap_or(0) == 0 {
                            return;
                        }
                        let mut line = String::new();
                        while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                            line.clear();
                        }
                        let path = first.split_whitespace().nth(1).unwrap_or_default();
                        let ours = token
                            .lock()
                            .unwrap()
                            .as_ref()
                            .is_some_and(|token| path.starts_with(&format!("/{token}/")));
                        let answer = if ours {
                            let body = format!("<served>{path}</served>");
                            format!(
                                "HTTP/1.1 207 Multi-Status\r\nContent-Length: {}\r\n\r\n{body}",
                                body.len()
                            )
                        } else {
                            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".to_string()
                        };
                        if out.write_all(answer.as_bytes()).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        *server = Some(address);
        address
    }
}

impl UserAndroid for FilesAndroid {
    fn state(&self) -> SessionState {
        *self.state.lock().unwrap()
    }
    fn last_reason(&self) -> Option<String> {
        None
    }
    fn start(&self) {
        *self.state.lock().unwrap() = SessionState::Active;
    }
    fn stop(&self) {
        *self.state.lock().unwrap() = SessionState::Stopped;
    }
    fn pointer(&self, _: &PointerEvent) -> Result<(), TaskRequestError> {
        Ok(())
    }
    fn key(&self, _: &KeyEvent) -> Result<(), TaskRequestError> {
        Ok(())
    }
    fn ime(&self, _: &ImeEvent) -> Result<(), TaskRequestError> {
        Ok(())
    }
    fn share_clipboard(&self, _: bool) {}
    fn host_clipboard(&self, _: String) {}
    fn watch_clipboard(&self) -> Option<std::sync::mpsc::Receiver<String>> {
        None
    }
    fn android_files_with_token(&self, token: &str) -> Result<String, String> {
        // As the session does: a stopped Android is refused, not started.
        if self.state() == SessionState::Stopped {
            return Err(dsa_host_core::session::ANDROID_STOPPED.to_string());
        }
        *self.token.lock().unwrap() = Some(token.to_string());
        Ok(format!("http://{}/{token}/", self.serve()))
    }
}

const KEY: &str = "0123456789abcdef";

fn door() -> (FilesDoor, Arc<FilesAndroid>) {
    let android = Arc::new(FilesAndroid::new());
    let androids = Arc::new(PerUser::new({
        let android = Arc::clone(&android);
        move |_: &str| Ok(android.clone() as Arc<dyn UserAndroid>)
    }));
    let owners: KeyOwners = Arc::new(|key: &str| (key == KEY).then(|| "S-1-5-21-1".to_string()));
    let door = FilesDoor::start(0, owners, androids, Duration::from_secs(5)).unwrap();
    (door, android)
}

/// Sends `request` and reads one answer: its status line and its body.
fn ask(stream: &mut TcpStream, request: &str) -> (String, String) {
    stream.write_all(request.as_bytes()).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).unwrap();
    (status.trim().to_string(), String::from_utf8(body).unwrap())
}

#[test]
fn fr29_the_files_door_answers_its_root_without_starting_android() {
    let (door, android) = door();
    let mut stream = TcpStream::connect(door.address()).unwrap();
    let (status, _) = ask(&mut stream, "OPTIONS / HTTP/1.1\r\n\r\n");
    assert_eq!(status, "HTTP/1.1 200 OK");
    let (status, body) = ask(
        &mut stream,
        "PROPFIND / HTTP/1.1\r\nDepth: 0\r\nContent-Length: 0\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 207 Multi-Status");
    assert!(body.contains("<D:collection/>"), "{body}");
    assert!(!body.contains(KEY), "the root names no key: {body}");
    assert_eq!(android.state(), SessionState::Stopped);
}

#[test]
fn fr29_the_files_door_refuses_a_key_nobody_has() {
    let (door, android) = door();
    let mut stream = TcpStream::connect(door.address()).unwrap();
    let (status, _) = ask(
        &mut stream,
        "PROPFIND /someoneelse/ HTTP/1.1\r\nDepth: 1\r\nContent-Length: 0\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    assert_eq!(android.state(), SessionState::Stopped);
}

#[test]
fn fr29_the_files_door_serves_a_running_androids_files_under_the_users_key() {
    let (door, android) = door();
    android.start();
    let mut stream = TcpStream::connect(door.address()).unwrap();
    let (status, body) = ask(
        &mut stream,
        &format!("PROPFIND /{KEY}/Pictures/ HTTP/1.1\r\nDepth: 1\r\nContent-Length: 0\r\n\r\n"),
    );
    assert_eq!(status, "HTTP/1.1 207 Multi-Status");
    assert_eq!(body, format!("<served>/{KEY}/Pictures/</served>"));
    assert_eq!(android.state(), SessionState::Active);
    // The same connection keeps going to Android.
    let (status, body) = ask(&mut stream, &format!("GET /{KEY}/a.txt HTTP/1.1\r\n\r\n"));
    assert_eq!(status, "HTTP/1.1 207 Multi-Status");
    assert_eq!(body, format!("<served>/{KEY}/a.txt</served>"));
}

#[test]
fn fr10_the_files_door_does_not_start_a_stopped_android() {
    // Explorer and the Web Client service reach the door without the user asking for Android.
    let (door, android) = door();
    let mut stream = TcpStream::connect(door.address()).unwrap();
    let asked = std::time::Instant::now();
    let (status, _) = ask(
        &mut stream,
        &format!("PROPFIND /{KEY}/ HTTP/1.1\r\nDepth: 1\r\nContent-Length: 0\r\n\r\n"),
    );
    assert_eq!(status, "HTTP/1.1 503 Service Unavailable");
    assert!(
        asked.elapsed() < Duration::from_secs(2),
        "a stopped Android is refused at once, not waited for"
    );
    assert_eq!(android.state(), SessionState::Stopped);
}
