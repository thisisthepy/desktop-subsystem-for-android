//! The clipboard loop every host runs in the user's session, against the local API's test
//! service.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::clipboard::{self, Clip, HostClipboard};
use dsa_host_core::host_api_conformance::{FakeAndroids, RunningService};
use dsa_transport::local;

const WAIT: Duration = Duration::from_secs(10);

/// A clipboard in memory that records what was put on it.
#[derive(Default)]
struct FakeClipboard {
    clip: Option<Clip>,
    set: Arc<Mutex<Vec<Clip>>>,
}

impl HostClipboard for FakeClipboard {
    fn read(&mut self) -> Option<Clip> {
        self.clip.clone()
    }
    fn write(&mut self, clip: &Clip) -> Result<(), String> {
        self.clip = Some(clip.clone());
        self.set.lock().unwrap().push(clip.clone());
        Ok(())
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn text(text: &str) -> Clip {
    Clip::Text(text.into())
}

/// A small image as PNG.
fn png(red: u8) -> Vec<u8> {
    let mut dib = vec![0u8; 40];
    dib[0] = 40;
    dib[4] = 1;
    dib[8] = 1;
    dib[12] = 1;
    dib[14] = 24;
    dib.extend_from_slice(&[0, 0, red, 0]);
    clipboard::dib_to_png(&dib).expect("a PNG")
}

struct Running {
    androids: Arc<FakeAndroids>,
    changes: mpsc::Sender<Clip>,
    set: Arc<Mutex<Vec<Clip>>>,
    stop: Arc<AtomicBool>,
    syncing: Option<thread::JoinHandle<()>>,
    _service: RunningService,
}

impl Running {
    fn start(first: Option<Clip>) -> Running {
        let (listener, connector) = local::pair();
        let androids = Arc::new(FakeAndroids::default());
        let service = RunningService::start(Box::new(listener), androids.clone());
        let (changes, receiver) = mpsc::channel();
        let set = Arc::new(Mutex::new(Vec::new()));
        let mut host = FakeClipboard {
            clip: first,
            set: Arc::clone(&set),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let syncing = thread::spawn(move || {
            clipboard::sync(
                Box::new(connector),
                "test clipboard",
                &mut host,
                receiver,
                flag,
                &|_| {},
            );
        });
        Running {
            androids,
            changes,
            set,
            stop,
            syncing: Some(syncing),
            _service: service,
        }
    }

    fn texts(&self) -> usize {
        self.androids
            .of("test")
            .map_or(0, |android| android.host_clipboard.lock().unwrap().len())
    }

    fn images(&self) -> usize {
        self.androids.of("test").map_or(0, |android| {
            android.host_clipboard_images.lock().unwrap().len()
        })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(syncing) = self.syncing.take() {
            let _ = syncing.join();
        }
    }
}

#[test]
fn fr13_the_host_carries_text_both_ways_without_echo() {
    let running = Running::start(Some(text("on the clipboard at the start")));
    // The clipboard's text at the start goes to the service.
    wait_until("the first text", || running.texts() == 1);
    let android = running.androids.of("test").unwrap();

    // Text copied on the host goes to the service.
    running.changes.send(text("copied on the host")).unwrap();
    wait_until("the copied text", || running.texts() == 2);

    // Text Android copied goes on the clipboard, and is not sent back when the clipboard says
    // it changed.
    android.copy_in_android("copied in Android");
    wait_until("the clipboard to be set", || {
        running.set.lock().unwrap().len() == 1
    });
    running.changes.send(text("copied in Android")).unwrap();
    running.changes.send(text("then the host again")).unwrap();
    wait_until("the next text", || running.texts() == 3);
    assert_eq!(
        *android.host_clipboard.lock().unwrap(),
        vec![
            "on the clipboard at the start".to_string(),
            "copied on the host".to_string(),
            "then the host again".to_string(),
        ]
    );
    assert_eq!(
        *running.set.lock().unwrap(),
        vec![text("copied in Android")]
    );
}

#[test]
fn fr13_text_longer_than_the_limit_is_not_passed_on() {
    let running = Running::start(None);
    running
        .changes
        .send(Clip::Text("x".repeat(clipboard::MAX_CLIPBOARD_BYTES + 1)))
        .unwrap();
    running.changes.send(text("short")).unwrap();
    wait_until("the short text", || running.texts() == 1);
    assert_eq!(
        *running
            .androids
            .of("test")
            .unwrap()
            .host_clipboard
            .lock()
            .unwrap(),
        vec!["short".to_string()]
    );
}

#[test]
fn fr13_an_image_copied_on_the_host_pastes_in_android() {
    let first = png(1);
    let running = Running::start(Some(Clip::Image(first.clone().into())));
    wait_until("the image at the start", || running.images() == 1);
    let copied = png(2);
    running
        .changes
        .send(Clip::Image(copied.clone().into()))
        .unwrap();
    wait_until("the copied image", || running.images() == 2);
    assert_eq!(
        *running
            .androids
            .of("test")
            .unwrap()
            .host_clipboard_images
            .lock()
            .unwrap(),
        vec![first, copied]
    );
}

#[test]
fn fr13_an_image_copied_in_android_pastes_on_the_host() {
    let running = Running::start(None);
    wait_until("the connection", || running.androids.of("test").is_some());
    let android = running.androids.of("test").unwrap();
    let image = png(3);
    // The service may not have taken the watch yet: copy until the clipboard is set.
    wait_until("the clipboard to hold the image", || {
        android.copy_image_in_android(&image);
        thread::sleep(Duration::from_millis(20));
        !running.set.lock().unwrap().is_empty()
    });
    assert_eq!(
        running.set.lock().unwrap()[0],
        Clip::Image(image.clone().into())
    );
    // The clipboard says it changed: the image is not sent back.
    running.changes.send(Clip::Image(image.into())).unwrap();
    running.changes.send(text("then text")).unwrap();
    wait_until("the text after it", || running.texts() == 1);
    assert_eq!(running.images(), 0);
}

#[test]
fn fr13_an_image_larger_than_the_cap_does_not_cross() {
    let running = Running::start(None);
    let mut huge = png(4);
    huge.resize(clipboard::MAX_IMAGE_BYTES + 1, 0);
    running.changes.send(Clip::Image(huge.into())).unwrap();
    running.changes.send(text("after the image")).unwrap();
    wait_until("the text after it", || running.texts() == 1);
    assert_eq!(running.images(), 0);
}

#[test]
fn fr13_text_wins_over_an_image_copied_with_it() {
    assert_eq!(
        clipboard::choose(Some("cells".into()), || panic!("the image is not read")),
        Some(text("cells"))
    );
    let image = png(5);
    assert_eq!(
        clipboard::choose(None, || Some(image.clone())),
        Some(Clip::Image(image.into()))
    );
    assert_eq!(clipboard::choose(None, || None), None);
}
