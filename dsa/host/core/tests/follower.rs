//! The helpers the followers of the user's Android share.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::follower::{spawn, wait, Log};

#[test]
fn d9_a_wait_ends_at_once_when_stop_is_set() {
    let stop = AtomicBool::new(true);
    let started = Instant::now();
    wait(&stop, Duration::from_secs(30));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn d9_a_wait_lasts_its_time_when_not_stopped() {
    let stop = AtomicBool::new(false);
    let started = Instant::now();
    wait(&stop, Duration::from_millis(120));
    assert!(started.elapsed() >= Duration::from_millis(120));
}

#[test]
fn d9_a_spawned_thread_runs_its_work() {
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&lines);
    let log: Log = Arc::new(move |line: &str| sink.lock().unwrap().push(line.to_string()));
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let handle = spawn("test-follower", &log, move || {
        flag.store(true, std::sync::atomic::Ordering::Relaxed)
    })
    .unwrap();
    handle.join().unwrap();
    assert!(ran.load(std::sync::atomic::Ordering::Relaxed));
    assert!(lines.lock().unwrap().is_empty());
}
