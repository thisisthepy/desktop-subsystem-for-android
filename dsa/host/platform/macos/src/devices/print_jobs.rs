//! The print jobs of the user's Android, printed on the Mac.
//!
//! The follower watches the print jobs over the local API ([`serve`]). Each job comes whole:
//! a PDF that Android's print dialog laid out, with the settings the user chose there. It is
//! handed to the [`Print`] function (on a Mac, the print panel through PDFKit), which lets
//! the user pick the printer, starting from Android's choices, and prints it. What became of
//! the job goes back to Android: printed, canceled in the panel, or failed with the reason. A
//! job Android withdraws before its panel opens is not printed. Nothing about a job ends the
//! follower.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dsa_host_core::print::{PrintJob, PrintOutcome};
use dsa_host_core::print_follower::{follow_print_jobs, PrintFollowing, PrintRun};
use dsa_transport::Connector;

use super::Log;

/// How the follower talks to the service.
const FOLLOWING: PrintFollowing = PrintFollowing {
    build: "dsa-devices print",
    answer: Duration::from_secs(5),
    retry: Duration::from_secs(1),
    poll: Duration::from_millis(100),
    thread_name: "dsa-print-job",
};

/// Prints one job and says what became of it. It may block until the user answers.
pub type Print = Arc<dyn Fn(PrintJob) -> PrintOutcome + Send + Sync>;

/// Prints `job` with `print` unless it was withdrawn first. A panic in the printing code is
/// a failed job, never the end of the follower.
pub fn print_one(job: PrintJob, print: &Print, withdrawn: &AtomicBool) -> PrintOutcome {
    if withdrawn.load(Ordering::Relaxed) {
        return PrintOutcome::Canceled;
    }
    if job.pdf.is_empty() {
        return PrintOutcome::Failed("the document is empty".into());
    }
    catch_unwind(AssertUnwindSafe(|| print(job)))
        .unwrap_or_else(|_| PrintOutcome::Failed("the printing code failed".into()))
}

/// Prints the documents of the user's Android until `stop`, reconnecting to the service
/// whenever the connection ends. Each job prints on a thread of its own.
pub fn serve(mut connector: Box<dyn Connector>, print: Print, stop: Arc<AtomicBool>, log: Log) {
    let run: PrintRun = Arc::new(move |job, withdrawn| print_one(job, &print, withdrawn));
    follow_print_jobs(
        &mut *connector,
        &FOLLOWING,
        &run,
        &stop,
        &|time| super::wait(&stop, time),
        &log,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_host_core::print::PrintSettings;

    fn job() -> PrintJob {
        PrintJob {
            job_id: 7,
            package_name: "com.example.notes".into(),
            app_label: "Notes".into(),
            document_name: "List".into(),
            settings: PrintSettings {
                copies: 2,
                ..PrintSettings::default()
            },
            pdf: Arc::new(b"%PDF-1.7\n".to_vec()),
        }
    }

    #[test]
    fn fr26_a_job_reaches_the_mac_with_its_settings() {
        let seen = Arc::new(std::sync::Mutex::new(None));
        let print: Print = {
            let seen = Arc::clone(&seen);
            Arc::new(move |job: PrintJob| {
                *seen.lock().unwrap() = Some(job);
                PrintOutcome::Printed
            })
        };
        let outcome = print_one(job(), &print, &AtomicBool::new(false));
        assert_eq!(outcome, PrintOutcome::Printed);
        assert_eq!(seen.lock().unwrap().as_ref(), Some(&job()));
    }

    #[test]
    fn fr26_a_job_withdrawn_before_its_panel_opens_is_not_printed() {
        let print: Print = Arc::new(|_| panic!("a withdrawn job was printed"));
        let outcome = print_one(job(), &print, &AtomicBool::new(true));
        assert_eq!(outcome, PrintOutcome::Canceled);
    }

    #[test]
    fn fr26_a_failure_in_the_printing_code_fails_only_that_job() {
        let print: Print = Arc::new(|_| panic!("the panel failed"));
        let outcome = print_one(job(), &print, &AtomicBool::new(false));
        assert!(matches!(outcome, PrintOutcome::Failed(_)));
    }
}
