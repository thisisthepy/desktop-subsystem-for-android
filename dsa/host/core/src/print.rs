//! Printing from a user's Android on the host's printers.
//!
//! An app in the guest prints as it would on a phone: Android's print dialog lays the document
//! out as a PDF and hands it to the agent's print service, which sends it to the host in pieces.
//! The host puts the pieces together ([`Assembler`]), checks them, and hands the whole job to
//! one watcher in the user's session, which shows the host's own print dialog so the user picks
//! a printer, then prints. What became of it (printed, canceled, failed) goes back to the guest,
//! which ends the job in its print spooler the same way.
//!
//! A job from the guest is checked as it comes: a document larger than [`MAX_PRINT_BYTES`],
//! pieces out of order or past the size, too many jobs at once, or a document that is not a PDF
//! fail the job in the guest, and the host holds no more than the limits here. Nothing a guest
//! sends ends the session.

use std::collections::HashMap;
use std::sync::Arc;

use dsa_protocol::control;

/// The largest document the host takes, in bytes. A printed PDF of a few hundred pages with
/// photos stays well under it; the whole document is held in memory until it prints.
pub const MAX_PRINT_BYTES: u64 = 128 * 1024 * 1024;

/// How many jobs may be on their way from the guest at once.
pub const MAX_INCOMING_JOBS: usize = 4;

/// The longest document name, app name or package the host keeps, in characters.
pub const MAX_NAME_CHARS: usize = 256;

/// The most copies the host asks a printer for.
pub const MAX_COPIES: u32 = 999;

/// How large each piece is on the local API, in bytes.
pub const PIECE_BYTES: usize = 1024 * 1024;

/// What the user chose for a job in Android's print dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrintSettings {
    /// 0 when the app did not say.
    pub page_count: u32,
    /// At least 1.
    pub copies: u32,
    pub color: bool,
    pub duplex: bool,
    pub landscape: bool,
    /// The paper's size, in thousandths of an inch; 0 when unknown.
    pub media_width_mils: u32,
    pub media_height_mils: u32,
}

/// One whole print job, ready to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrintJob {
    /// The host's number for the job, unique while the service runs.
    pub job_id: u32,
    pub package_name: String,
    pub app_label: String,
    pub document_name: String,
    pub settings: PrintSettings,
    /// The document, a PDF.
    pub pdf: Arc<Vec<u8>>,
}

impl PrintJob {
    /// The name to show for the job: the document's, else the app's, else a plain one.
    pub fn title(&self) -> String {
        [&self.document_name, &self.app_label, &self.package_name]
            .into_iter()
            .find(|name| !name.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| "Android document".into())
    }
}

/// A change to the guest's print jobs, as a watcher hears it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrintEvent {
    /// A job to print, which only this watcher was given.
    Job(PrintJob),
    /// The job is gone from the guest: canceled there, or the guest stopped. It is not to be
    /// printed, and its outcome is not wanted.
    Withdrawn { job_id: u32 },
}

/// What became of a print job on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrintOutcome {
    Printed,
    /// The user canceled the host's print dialog.
    Canceled,
    /// It could not be printed, for this reason.
    Failed(String),
}

impl PrintOutcome {
    /// The guest's message for this outcome.
    pub fn to_control(&self, job_id: u32) -> control::PrintJobResult {
        let (outcome, message) = match self {
            PrintOutcome::Printed => (control::PrintOutcome::Printed, String::new()),
            PrintOutcome::Canceled => (control::PrintOutcome::Canceled, String::new()),
            PrintOutcome::Failed(why) => (control::PrintOutcome::Failed, cut(why, 1024)),
        };
        control::PrintJobResult {
            job_id,
            outcome: outcome as i32,
            message,
        }
    }
}

/// What the session does after the assembler took a message from the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Nothing yet: more pieces are to come, or the message changed nothing.
    Wait,
    /// The job is whole: hand it to a watcher.
    Complete(PrintJob),
    /// The guest's job `guest_id` is refused: tell the guest, for this reason.
    Refuse { guest_id: u32, why: String },
    /// The job the watcher has was canceled in the guest: withdraw it.
    Withdraw { job_id: u32 },
}

/// A job whose document is still arriving.
struct Incoming {
    package_name: String,
    app_label: String,
    document_name: String,
    settings: PrintSettings,
    size: u64,
    pdf: Vec<u8>,
}

/// Puts the guest's print jobs together from its pieces, and remembers which guest job each
/// handed-on job is, so the outcome reaches the right one. Platform neutral; the session owns
/// one and clears it when the guest's connection ends.
#[derive(Default)]
pub struct Assembler {
    incoming: HashMap<u32, Incoming>,
    /// Handed-on jobs: the host's number to the guest's.
    handed: HashMap<u32, u32>,
    last_id: u32,
}

/// Cuts `text` to at most `max` characters.
fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

impl Assembler {
    pub fn new() -> Assembler {
        Assembler::default()
    }

    /// The guest announced a job.
    pub fn start(&mut self, job: control::PrintJob) -> Step {
        let refuse = |why: String| Step::Refuse {
            guest_id: job.job_id,
            why,
        };
        if self.incoming.contains_key(&job.job_id)
            || self.handed.values().any(|g| *g == job.job_id)
        {
            return refuse(format!("print job {} is already under way", job.job_id));
        }
        if job.size == 0 {
            return refuse("the document is empty".into());
        }
        if job.size > MAX_PRINT_BYTES {
            return refuse(format!(
                "the document is {} MiB; the host prints documents of at most {} MiB",
                job.size.div_ceil(1024 * 1024),
                MAX_PRINT_BYTES / (1024 * 1024)
            ));
        }
        if self.incoming.len() >= MAX_INCOMING_JOBS {
            return refuse(format!(
                "{MAX_INCOMING_JOBS} documents are already on their way to the host"
            ));
        }
        let settings = PrintSettings {
            page_count: job.page_count,
            copies: job.copies.clamp(1, MAX_COPIES),
            color: job.color,
            duplex: job.duplex,
            landscape: job.landscape,
            media_width_mils: job.media_width_mils,
            media_height_mils: job.media_height_mils,
        };
        self.incoming.insert(
            job.job_id,
            Incoming {
                package_name: cut(&job.package_name, MAX_NAME_CHARS),
                app_label: cut(&job.app_label, MAX_NAME_CHARS),
                document_name: cut(&job.document_name, MAX_NAME_CHARS),
                settings,
                size: job.size,
                pdf: Vec::new(),
            },
        );
        Step::Wait
    }

    /// One piece of a job's document came.
    pub fn chunk(&mut self, chunk: control::PrintJobChunk) -> Step {
        let guest_id = chunk.job_id;
        let Some(incoming) = self.incoming.get_mut(&guest_id) else {
            return Step::Refuse {
                guest_id,
                why: format!("the host has no print job {guest_id}"),
            };
        };
        let received = incoming.pdf.len() as u64;
        let fault = if chunk.offset != received {
            let offset = chunk.offset;
            Some(format!("a piece came at byte {offset} where byte {received} was due"))
        } else if received + chunk.data.len() as u64 > incoming.size {
            Some(format!(
                "the document is longer than the {} bytes it was announced at",
                incoming.size
            ))
        } else {
            None
        };
        if let Some(why) = fault {
            self.incoming.remove(&guest_id);
            return Step::Refuse { guest_id, why };
        }
        incoming.pdf.extend_from_slice(&chunk.data);
        if (incoming.pdf.len() as u64) < incoming.size {
            return Step::Wait;
        }
        let Some(done) = self.incoming.remove(&guest_id) else {
            return Step::Wait;
        };
        if !done.pdf.starts_with(b"%PDF-") {
            return Step::Refuse {
                guest_id,
                why: "the document is not a PDF".into(),
            };
        }
        self.last_id = self.last_id.wrapping_add(1).max(1);
        let job_id = self.last_id;
        self.handed.insert(job_id, guest_id);
        Step::Complete(PrintJob {
            job_id,
            package_name: done.package_name,
            app_label: done.app_label,
            document_name: done.document_name,
            settings: done.settings,
            pdf: Arc::new(done.pdf),
        })
    }

    /// The guest canceled a job.
    pub fn cancel(&mut self, guest_id: u32) -> Step {
        if self.incoming.remove(&guest_id).is_some() {
            return Step::Wait;
        }
        let handed = self
            .handed
            .iter()
            .find(|(_, guest)| **guest == guest_id)
            .map(|(job, _)| *job);
        match handed {
            Some(job_id) => {
                self.handed.remove(&job_id);
                Step::Withdraw { job_id }
            }
            None => Step::Wait,
        }
    }

    /// The watcher answered the job: the guest's number for it, or `None` for a job the guest
    /// no longer has.
    pub fn answered(&mut self, job_id: u32) -> Option<u32> {
        self.handed.remove(&job_id)
    }

    /// The guest's connection ended: drops what was arriving, and returns the handed-on jobs,
    /// which are to be withdrawn.
    pub fn clear(&mut self) -> Vec<u32> {
        self.incoming.clear();
        let mut gone: Vec<u32> = self.handed.drain().map(|(job, _)| job).collect();
        gone.sort_unstable();
        gone
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(guest_id: u32, size: u64) -> control::PrintJob {
        control::PrintJob {
            job_id: guest_id,
            package_name: "com.example.docs".into(),
            app_label: "Docs".into(),
            document_name: "Report".into(),
            size,
            page_count: 2,
            copies: 1,
            color: true,
            duplex: false,
            landscape: false,
            media_width_mils: 8500,
            media_height_mils: 11000,
        }
    }

    fn piece(guest_id: u32, offset: u64, data: &[u8]) -> control::PrintJobChunk {
        control::PrintJobChunk {
            job_id: guest_id,
            offset,
            data: data.to_vec(),
        }
    }

    /// The guest job a step refuses, if it refuses one.
    fn refused(step: Step) -> Option<u32> {
        match step {
            Step::Refuse { guest_id, .. } => Some(guest_id),
            _ => None,
        }
    }

    const PDF: &[u8] = b"%PDF-1.7 a small document";

    #[test]
    fn fr26_a_document_in_pieces_becomes_one_job() {
        let mut assembler = Assembler::new();
        assert_eq!(assembler.start(job(7, PDF.len() as u64)), Step::Wait);
        assert_eq!(assembler.chunk(piece(7, 0, &PDF[..5])), Step::Wait);
        let Step::Complete(done) = assembler.chunk(piece(7, 5, &PDF[5..])) else {
            panic!("the job is whole");
        };
        assert_eq!(*done.pdf, PDF);
        assert_eq!(done.document_name, "Report");
        assert_eq!(done.settings.media_width_mils, 8500);
        assert_eq!(assembler.answered(done.job_id), Some(7));
        assert_eq!(assembler.answered(done.job_id), None, "answered once");
    }

    #[test]
    fn fr26_a_document_past_the_limit_is_refused_before_it_is_sent() {
        let mut assembler = Assembler::new();
        assert_eq!(refused(assembler.start(job(1, MAX_PRINT_BYTES + 1))), Some(1));
        assert_eq!(refused(assembler.start(job(2, 0))), Some(2));
    }

    #[test]
    fn fr26_pieces_out_of_order_or_past_the_size_fail_the_job() {
        let mut assembler = Assembler::new();
        assembler.start(job(1, 10));
        assert_eq!(refused(assembler.chunk(piece(1, 4, b"abcd"))), Some(1));
        assembler.start(job(2, 4));
        assert_eq!(refused(assembler.chunk(piece(2, 0, b"%PDF-"))), Some(2));
        assert_eq!(refused(assembler.chunk(piece(9, 0, b"x"))), Some(9));
    }

    #[test]
    fn fr26_a_document_that_is_not_a_pdf_is_refused() {
        let mut assembler = Assembler::new();
        assembler.start(job(1, 4));
        assert_eq!(refused(assembler.chunk(piece(1, 0, b"MZ\0\0"))), Some(1));
    }

    #[test]
    fn fr26_too_many_jobs_at_once_are_refused() {
        let mut assembler = Assembler::new();
        for id in 0..MAX_INCOMING_JOBS as u32 {
            assert_eq!(assembler.start(job(id, 100)), Step::Wait);
        }
        assert_eq!(refused(assembler.start(job(99, 100))), Some(99));
    }

    #[test]
    fn fr26_a_job_canceled_in_the_guest_is_withdrawn_once_handed_on() {
        let mut assembler = Assembler::new();
        assembler.start(job(3, 100));
        assert_eq!(assembler.cancel(3), Step::Wait, "dropped while arriving");
        assembler.start(job(4, PDF.len() as u64));
        let Step::Complete(done) = assembler.chunk(piece(4, 0, PDF)) else {
            panic!("the job is whole");
        };
        assert_eq!(
            assembler.cancel(4),
            Step::Withdraw {
                job_id: done.job_id
            }
        );
        assert_eq!(assembler.answered(done.job_id), None);
    }

    #[test]
    fn fr26_the_guest_going_away_withdraws_every_handed_on_job() {
        let mut assembler = Assembler::new();
        assembler.start(job(4, PDF.len() as u64));
        let Step::Complete(done) = assembler.chunk(piece(4, 0, PDF)) else {
            panic!("the job is whole");
        };
        assembler.start(job(5, 100));
        assert_eq!(assembler.clear(), vec![done.job_id]);
        assert_eq!(refused(assembler.chunk(piece(5, 0, b"x"))), Some(5));
    }

    #[test]
    fn fr26_each_outcome_reaches_the_guest_as_its_result() {
        let printed = PrintOutcome::Printed.to_control(3);
        assert_eq!(printed.job_id, 3);
        assert_eq!(printed.outcome, control::PrintOutcome::Printed as i32);
        assert_eq!(
            PrintOutcome::Canceled.to_control(3).outcome,
            control::PrintOutcome::Canceled as i32
        );
        let failed = PrintOutcome::Failed("out of paper".into()).to_control(3);
        assert_eq!(failed.outcome, control::PrintOutcome::Failed as i32);
        assert_eq!(failed.message, "out of paper");
    }
}
