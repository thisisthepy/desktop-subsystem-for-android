//! The window manager's side of printing: each job the service hands it is written to the
//! spool folder, printed, removed, and what became of it goes back to the service. The service
//! runs over the in-memory local transport, with a fake Android; the printer is a fake too.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::host_api_conformance::{
    sample_print_job, FakeAndroid, FakeAndroids, RunningService,
};
use dsa_host_core::print::{PrintEvent, PrintJob, PrintOutcome};
use dsa_host_windows::compositor::print::{print_one, serve, Printer};
use dsa_transport::local;

const WAIT: Duration = Duration::from_secs(10);

/// Records each document it is given, and answers with `outcome`. With `hold`, it waits until
/// the job is withdrawn, as a dialog left open would.
struct Recorded {
    documents: Mutex<Vec<(u32, Vec<u8>, PathBuf)>>,
    outcome: PrintOutcome,
    hold: bool,
}

impl Recorded {
    fn new(outcome: PrintOutcome, hold: bool) -> Arc<Recorded> {
        Arc::new(Recorded {
            documents: Mutex::new(Vec::new()),
            outcome,
            hold,
        })
    }
}

impl Printer for Recorded {
    fn print(&self, job: &PrintJob, document: &Path, withdrawn: &AtomicBool) -> PrintOutcome {
        let content = std::fs::read(document).unwrap_or_default();
        let entry = (job.job_id, content, document.to_path_buf());
        self.documents.lock().unwrap().push(entry);
        if self.hold {
            let deadline = Instant::now() + WAIT;
            while !withdrawn.load(Ordering::Relaxed) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            return PrintOutcome::Canceled;
        }
        self.outcome.clone()
    }
}

fn spool(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dsa-print-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Runs the window manager's printing against a fake Android, and returns it.
fn printing(
    printer: Arc<Recorded>,
    dir: PathBuf,
) -> (
    Arc<FakeAndroid>,
    Arc<AtomicBool>,
    thread::JoinHandle<()>,
    RunningService,
) {
    let (listener, connector) = local::pair();
    let androids = Arc::new(FakeAndroids::default());
    let service = RunningService::start(Box::new(listener), androids.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let serving = {
        let stop = Arc::clone(&stop);
        thread::spawn(move || serve(Box::new(connector), printer, Some(dir), stop, None))
    };
    let deadline = Instant::now() + WAIT;
    let android = loop {
        if let Some(android) = androids.androids.lock().unwrap().values().next().cloned() {
            break android;
        }
        assert!(Instant::now() < deadline, "the print jobs were never watched");
        thread::sleep(Duration::from_millis(10));
    };
    (android, stop, serving, service)
}

/// Hands `job` to the window manager once its watch is up.
fn hand(android: &FakeAndroid, job: PrintJob) {
    let deadline = Instant::now() + WAIT;
    while !android.print(PrintEvent::Job(job.clone())) {
        assert!(Instant::now() < deadline, "nobody took the print job");
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_result(android: &FakeAndroid) -> (u32, PrintOutcome) {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(result) = android.print_results.lock().unwrap().first().cloned() {
            return result;
        }
        assert!(Instant::now() < deadline, "no outcome reached Android");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn fr26_a_job_is_printed_from_a_spool_file_that_is_removed_after() {
    let printer = Recorded::new(PrintOutcome::Printed, false);
    let dir = spool("printed");
    let (android, stop, serving, _service) = printing(printer.clone(), dir.clone());
    let job = sample_print_job(4, 3000);
    hand(&android, job.clone());
    assert_eq!(wait_for_result(&android), (4, PrintOutcome::Printed));
    let documents = printer.documents.lock().unwrap().clone();
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].1, *job.pdf, "the whole document");
    assert!(documents[0].2.starts_with(&dir), "in the spool folder");
    assert!(!documents[0].2.exists(), "removed once printed");
    stop.store(true, Ordering::Relaxed);
    serving.join().unwrap();
}

#[test]
fn fr26_canceling_the_dialog_cancels_the_job_in_android() {
    let printer = Recorded::new(PrintOutcome::Canceled, false);
    let (android, stop, serving, _service) = printing(printer, spool("canceled"));
    hand(&android, sample_print_job(2, 100));
    assert_eq!(wait_for_result(&android), (2, PrintOutcome::Canceled));
    stop.store(true, Ordering::Relaxed);
    serving.join().unwrap();
}

#[test]
fn fr26_a_printer_failure_reaches_android_with_its_reason() {
    let why = PrintOutcome::Failed("the printer is offline".into());
    let printer = Recorded::new(why.clone(), false);
    let (android, stop, serving, _service) = printing(printer, spool("failed"));
    hand(&android, sample_print_job(3, 100));
    assert_eq!(wait_for_result(&android), (3, why));
    stop.store(true, Ordering::Relaxed);
    serving.join().unwrap();
}

#[test]
fn fr26_a_job_withdrawn_while_its_dialog_is_open_is_not_printed() {
    let printer = Recorded::new(PrintOutcome::Printed, true);
    let (android, stop, serving, _service) = printing(printer.clone(), spool("withdrawn"));
    hand(&android, sample_print_job(6, 100));
    let deadline = Instant::now() + WAIT;
    while printer.documents.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the job never reached the printer");
        thread::sleep(Duration::from_millis(10));
    }
    android.print(PrintEvent::Withdrawn { job_id: 6 });
    assert_eq!(wait_for_result(&android), (6, PrintOutcome::Canceled));
    stop.store(true, Ordering::Relaxed);
    serving.join().unwrap();
}

/// Panics, as a broken printer driver might take the printing code down.
struct Panicking;

impl Printer for Panicking {
    fn print(&self, _: &PrintJob, _: &Path, _: &AtomicBool) -> PrintOutcome {
        panic!("a broken driver");
    }
}

#[test]
fn nfr5_a_printer_that_fails_badly_fails_the_job_and_nothing_else() {
    let dir = spool("panic");
    let withdrawn = AtomicBool::new(false);
    let outcome = print_one(&sample_print_job(1, 100), &Panicking, Some(&dir), &withdrawn);
    assert!(matches!(outcome, PrintOutcome::Failed(_)));
    let left = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
    assert_eq!(left, 0, "the document is removed");
}

#[test]
fn nfr5_a_job_without_a_spool_folder_fails_in_android() {
    let withdrawn = AtomicBool::new(false);
    let printer = Recorded::new(PrintOutcome::Printed, false);
    let outcome = print_one(&sample_print_job(1, 100), &*printer, None, &withdrawn);
    assert!(matches!(outcome, PrintOutcome::Failed(_)));
    assert!(printer.documents.lock().unwrap().is_empty());
}
