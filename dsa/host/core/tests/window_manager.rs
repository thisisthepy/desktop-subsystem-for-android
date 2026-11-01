//! When the service starts a user's window manager, against a fake of the platform.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::window_manager::{WindowManagerHost, WindowManagerKeeper};

#[derive(Default)]
struct Fake {
    running: bool,
    starts: u32,
    cannot_start: Option<String>,
    comes_up: bool,
}

#[derive(Clone, Default)]
struct Host(Arc<Mutex<Fake>>);

impl WindowManagerHost for Host {
    fn running(&mut self) -> bool {
        self.0.lock().unwrap().running
    }
    fn start(&mut self) -> Result<(), String> {
        let mut fake = self.0.lock().unwrap();
        if let Some(why) = fake.cannot_start.clone() {
            return Err(why);
        }
        fake.starts += 1;
        fake.running = fake.comes_up;
        Ok(())
    }
}

fn seconds(count: u64) -> Duration {
    Duration::from_secs(count)
}

#[test]
fn fr3_a_running_window_manager_is_left_alone() {
    let host = Host::default();
    host.0.lock().unwrap().running = true;
    let mut keeper = WindowManagerKeeper::new(host.clone());
    assert_eq!(keeper.check(Instant::now()), None);
    assert_eq!(host.0.lock().unwrap().starts, 0);
}

#[test]
fn fr3_a_window_manager_that_is_not_running_is_started_once_and_given_time() {
    let host = Host::default();
    let mut keeper = WindowManagerKeeper::new(host.clone());
    let now = Instant::now();
    let said = keeper.check(now).expect("Android has to wait for it");
    assert!(said.contains("starting"), "{said}");
    assert!(keeper.check(now + seconds(1)).is_some());
    assert_eq!(host.0.lock().unwrap().starts, 1);

    host.0.lock().unwrap().running = true;
    assert_eq!(keeper.check(now + seconds(2)), None);
}

#[test]
fn fr3_a_window_manager_that_dies_while_android_runs_is_started_again() {
    let host = Host::default();
    host.0.lock().unwrap().comes_up = true;
    let mut keeper = WindowManagerKeeper::new(host.clone());
    let now = Instant::now();
    assert!(keeper.check(now).is_some());
    assert_eq!(keeper.check(now + seconds(1)), None);

    host.0.lock().unwrap().running = false;
    assert!(keeper.check(now + seconds(2)).is_some());
    assert_eq!(host.0.lock().unwrap().starts, 2);
    assert_eq!(keeper.check(now + seconds(3)), None);
}

#[test]
fn fr3_a_window_manager_that_keeps_exiting_is_retried_more_slowly_and_reported() {
    let host = Host::default();
    let mut keeper = WindowManagerKeeper::new(host.clone());
    let begin = Instant::now();
    let mut said = String::new();
    for second in 0..600 {
        if let Some(why) = keeper.check(begin + seconds(second)) {
            said = why;
        }
    }
    let starts = host.0.lock().unwrap().starts;
    assert!((5..=15).contains(&starts), "{starts} starts in ten minutes");
    assert!(said.contains("keeps exiting"), "{said}");
}

#[test]
fn fr3_a_window_manager_that_cannot_be_started_says_why_and_is_tried_again() {
    let host = Host::default();
    host.0.lock().unwrap().cannot_start = Some("no one is signed in".into());
    let mut keeper = WindowManagerKeeper::new(host.clone());
    let now = Instant::now();
    let said = keeper.check(now).expect("Android has to wait for it");
    assert!(said.contains("no one is signed in"), "{said}");

    host.0.lock().unwrap().cannot_start = None;
    assert!(keeper.check(now + seconds(1)).is_some());
    assert_eq!(host.0.lock().unwrap().starts, 0, "not yet");
    assert!(keeper.check(now + seconds(6)).is_some());
    assert_eq!(host.0.lock().unwrap().starts, 1);
}
