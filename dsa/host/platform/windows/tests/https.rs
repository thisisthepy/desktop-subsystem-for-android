//! WinHTTP downloads, against a small HTTP server on this machine (Windows only).
#![cfg(windows)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use dsa_host_core::update::store::Fetch;
use dsa_host_windows::https::WinHttpFetch;

/// Answers one request with `status` and `body`, and returns the request line it got.
fn serve_once(status: &'static str, body: Vec<u8>) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        let header = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
        String::from_utf8_lossy(&request)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    });
    (format!("http://{address}"), server)
}

#[test]
fn fr14_an_image_file_is_downloaded_whole() {
    let body: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let (base, server) = serve_once("200 OK", body.clone());
    let mut out = Vec::new();
    let written = WinHttpFetch::default()
        .fetch(&format!("{base}/16.0.2/system.vhdx?x=1"), &mut out)
        .unwrap();
    assert_eq!(written, body.len() as u64);
    assert!(out == body, "the download arrived changed");
    assert_eq!(
        server.join().unwrap(),
        "GET /16.0.2/system.vhdx?x=1 HTTP/1.1"
    );
}

#[test]
fn fr14_a_missing_file_is_an_error_naming_the_status() {
    let (base, server) = serve_once("404 Not Found", b"no".to_vec());
    let error = WinHttpFetch::default()
        .fetch(&format!("{base}/nothing"), &mut Vec::new())
        .unwrap_err();
    assert!(error.contains("HTTP 404"), "{error}");
    server.join().unwrap();
}

#[test]
fn fr14_other_schemes_are_refused() {
    let error = WinHttpFetch::default()
        .fetch("ftp://example.invalid/x", &mut Vec::new())
        .unwrap_err();
    assert!(!error.is_empty());
}

#[test]
fn fr15_a_mirror_on_a_drive_is_read_as_a_file_and_the_web_through_winhttp() {
    use dsa_host_windows::https::UrlFetch;
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("mirror {}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("program.json");
    std::fs::write(&file, b"{}").unwrap();
    let mut body = Vec::new();
    UrlFetch::default()
        .fetch(file.to_str().unwrap(), &mut body)
        .unwrap();
    assert_eq!(body, b"{}");

    let (base, server) = serve_once("200 OK", b"from the web".to_vec());
    let mut body = Vec::new();
    UrlFetch::default()
        .fetch(&format!("{base}/program.json"), &mut body)
        .unwrap();
    assert_eq!(body, b"from the web");
    assert_eq!(server.join().unwrap(), "GET /program.json HTTP/1.1");
}
