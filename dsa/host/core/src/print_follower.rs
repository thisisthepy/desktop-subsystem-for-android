//! The follower that prints the documents of the user's Android on the host, the same on every
//! host.
//!
//! It watches the print jobs over the local API and reconnects whenever the connection ends.
//! Each job prints on a thread of its own, so the watch goes on while a print dialog is open.
//! What became of a job goes back to Android through a new connection. A job Android withdraws
//! is told so, and one still printing when the follower stops is told it is not wanted. What
//! prints a job (the host's own dialog) is the host's [`PrintRun`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_transport::Connector;

use crate::host_api::HostApiClient;
use crate::print::{PrintEvent, PrintJob, PrintOutcome};

/// Prints one job and says what became of it. `withdrawn` turns true when Android no longer
/// wants the job. It may block until the user answers.
pub type PrintRun = Arc<dyn Fn(PrintJob, &AtomicBool) -> PrintOutcome + Send + Sync>;

/// Where a message to the log goes.
pub type PrintLog = Arc<dyn Fn(&str) + Send + Sync>;

/// How a host's follower talks to the service.
pub struct PrintFollowing {
    /// What the follower calls itself to the service.
    pub build: &'static str,
    /// How long a request to the service is waited for.
    pub answer: Duration,
    /// How long to wait before connecting again after the service could not be reached.
    pub retry: Duration,
    /// How long the watch waits for the service before it looks for finished jobs again.
    pub poll: Duration,
    /// The name of each job's thread.
    pub thread_name: &'static str,
}

/// Prints the documents of the user's Android until `stop`. `wait` is how the host waits out
/// the retry time (it may end early when `stop` is set).
pub fn follow_print_jobs(
    connector: &mut dyn Connector,
    following: &PrintFollowing,
    run: &PrintRun,
    stop: &AtomicBool,
    wait: &dyn Fn(Duration),
    log: &PrintLog,
) {
    let (done_tx, done) = mpsc::channel();
    let mut printing: HashMap<u32, Arc<AtomicBool>> = HashMap::new();
    let mut last_error = String::new();
    while !stop.load(Ordering::Relaxed) {
        let stream = HostApiClient::connect(&mut *connector, following.build, following.answer)
            .and_then(|client| client.watch_print_jobs());
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                let error = error.to_string();
                if error != last_error {
                    log(&format!("printing cannot reach the service: {error}"));
                    last_error = error;
                }
                report(connector, following, &done, &mut printing, log);
                wait(following.retry);
                continue;
            }
        };
        last_error.clear();
        while !stop.load(Ordering::Relaxed) {
            report(connector, following, &done, &mut printing, log);
            match stream.next(following.poll) {
                Ok(Some(PrintEvent::Job(job))) => {
                    let withdrawn = Arc::new(AtomicBool::new(false));
                    printing.insert(job.job_id, Arc::clone(&withdrawn));
                    start_job(
                        following.thread_name,
                        job,
                        Arc::clone(run),
                        withdrawn,
                        done_tx.clone(),
                        Arc::clone(log),
                    );
                }
                Ok(Some(PrintEvent::Withdrawn { job_id })) => {
                    if let Some(withdrawn) = printing.get(&job_id) {
                        withdrawn.store(true, Ordering::Relaxed);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    log(&format!(
                        "the print jobs' connection to the service ended: {error}"
                    ));
                    break;
                }
            }
        }
    }
    // Jobs still printing are told they are not wanted; their dialogs stay until the user
    // closes them.
    for withdrawn in printing.values() {
        withdrawn.store(true, Ordering::Relaxed);
    }
}

/// Passes what became of each finished job to the service.
fn report(
    connector: &mut dyn Connector,
    following: &PrintFollowing,
    done: &Receiver<(u32, PrintOutcome)>,
    printing: &mut HashMap<u32, Arc<AtomicBool>>,
    log: &PrintLog,
) {
    while let Ok((job_id, outcome)) = done.try_recv() {
        printing.remove(&job_id);
        let result = HostApiClient::connect(connector, following.build, following.answer)
            .and_then(|mut client| client.report_print_job(job_id, &outcome));
        if let Err(error) = result {
            log(&format!(
                "what became of print job {job_id} did not reach Android: {error}"
            ));
        }
    }
}

/// Prints one job on a thread of its own, and sends what became of it on `done`.
fn start_job(
    thread_name: &'static str,
    job: PrintJob,
    run: PrintRun,
    withdrawn: Arc<AtomicBool>,
    done: Sender<(u32, PrintOutcome)>,
    log: PrintLog,
) {
    let job_id = job.job_id;
    let failed = done.clone();
    let spawned = thread::Builder::new()
        .name(thread_name.into())
        .spawn(move || {
            let title = job.title();
            let outcome = run(job, &withdrawn);
            match &outcome {
                PrintOutcome::Failed(why) => log(&format!("{title} was not printed: {why}")),
                PrintOutcome::Canceled => log(&format!("printing {title} was canceled")),
                PrintOutcome::Printed => {}
            }
            let _ = done.send((job_id, outcome));
        });
    if let Err(error) = spawned {
        let why = format!("printing could not start: {error}");
        let _ = failed.send((job_id, PrintOutcome::Failed(why)));
    }
}
