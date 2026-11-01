//! The window manager's side of web links: it opens only http and https links, whatever the
//! service sends. The service runs over the in-memory local transport, with a fake Android.
#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::host_api_conformance::{FakeAndroids, RunningService};
use dsa_host_windows::compositor::links::{follow, openable, LinkOpener};
use dsa_transport::local;

const WAIT: Duration = Duration::from_secs(10);

#[test]
fn fr9_the_window_manager_accepts_only_http_and_https_links() {
    assert_eq!(
        openable("https://example.com/a").unwrap(),
        "https://example.com/a"
    );
    assert!(openable("http://example.com").is_ok());
    for bad in [
        "file:///C:/Windows/System32/calc.exe",
        "C:\\Windows\\System32\\calc.exe",
        "\\\\server\\share\\run.exe",
        "calc",
        "ms-settings:display",
        "shell:startup",
        "javascript:alert(1)",
        "https://example.com/\" --new-window \"file:///C:/",
        "",
    ] {
        assert!(openable(bad).is_err(), "{bad:?} would be opened");
    }
}

/// Remembers the links it was asked to open.
#[derive(Clone, Default)]
struct Recorded(Arc<Mutex<Vec<String>>>);

impl LinkOpener for Recorded {
    fn open(&mut self, url: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(url.to_string());
        Ok(())
    }
}

#[test]
fn fr9_links_from_the_service_open_and_others_are_dropped() {
    let (listener, connector) = local::pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(Box::new(listener), androids.clone());
    let opened = Recorded::default();
    let stop = Arc::new(AtomicBool::new(false));
    let following = {
        let mut opener = opened.clone();
        let stop = Arc::clone(&stop);
        thread::spawn(move || follow(Box::new(connector), &mut opener, stop, None))
    };
    // The watch is up once the service has made the caller's Android for it.
    let deadline = Instant::now() + WAIT;
    let android = loop {
        if let Some(android) = androids.androids.lock().unwrap().values().next().cloned() {
            break android;
        }
        assert!(Instant::now() < deadline, "the links were never watched");
        thread::sleep(Duration::from_millis(10));
    };
    thread::sleep(Duration::from_millis(100));
    android.open_link_in_android("file:///C:/Windows/System32/calc.exe");
    android.open_link_in_android("https://example.com/page");
    let deadline = Instant::now() + WAIT;
    while opened.0.lock().unwrap().is_empty() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(100));
    stop.store(true, Ordering::Relaxed);
    following.join().unwrap();
    assert_eq!(
        *opened.0.lock().unwrap(),
        vec!["https://example.com/page".to_string()]
    );
}
