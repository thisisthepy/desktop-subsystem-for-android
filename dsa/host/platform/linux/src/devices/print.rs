//! Printing the documents apps in the user's Android print, on the user's own printers.
//!
//! The follower watches the print jobs of the user's Android over the local API ([`serve`]).
//! Each job comes whole: a PDF that Android's print dialog laid out, with the settings the
//! user chose there. It is written to the user's spool folder and handed to the desktop's
//! print dialog through the print portal (`org.freedesktop.portal.Print`), which lets the
//! user pick a printer, pages and copies, starting from Android's choices, and prints it.
//! Where no portal answers, the job goes to the default CUPS printer with `lp`, with
//! Android's settings.
//!
//! What became of the job goes back to Android: printed, canceled in the dialog, or failed
//! with the reason. A job Android withdraws before its dialog opens is not printed. Nothing
//! about a job ends the follower.

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_core::host_paths::unique_name;
use dsa_host_core::print::{print_spooled, PrintJob, PrintOutcome, PrintSettings};
use dsa_host_core::print_follower::{follow_print_jobs, PrintFollowing, PrintRun};
use dsa_transport::Connector;
use zbus::blocking::Connection;
use zbus::zvariant::{Fd, OwnedObjectPath, OwnedValue, Value};

use super::dbus::proxy;
use super::Log;

/// How the follower talks to the service.
const FOLLOWING: PrintFollowing = PrintFollowing {
    build: "dsa-devices print",
    answer: Duration::from_secs(5),
    retry: Duration::from_secs(1),
    poll: Duration::from_millis(100),
    thread_name: "dsa-print-job",
};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PRINT: &str = "org.freedesktop.portal.Print";
const REQUEST: &str = "org.freedesktop.portal.Request";

/// Prints one document.
pub trait Printer: Send + Sync {
    /// Prints `job`, whose PDF is the file `document`, and says what became of it.
    /// `withdrawn` turns true when Android no longer wants it.
    fn print(&self, job: &PrintJob, document: &Path, withdrawn: &AtomicBool) -> PrintOutcome;
}

/// The folder documents wait in while they print: the user's runtime folder, which only the
/// user can read and which the system empties at logout.
pub fn default_spool_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("dsa").join("print"))
}

/// Prints the documents of the user's Android until `stop`, reconnecting to the service
/// whenever the connection ends. Each job prints on a thread of its own.
pub fn serve(
    mut connector: Box<dyn Connector>,
    printer: Arc<dyn Printer>,
    spool: Option<PathBuf>,
    stop: Arc<AtomicBool>,
    log: Log,
) {
    let run: PrintRun =
        Arc::new(move |job, withdrawn| print_one(&job, &*printer, spool.as_deref(), withdrawn));
    follow_print_jobs(
        &mut *connector,
        &FOLLOWING,
        &run,
        &stop,
        &thread::sleep,
        &log,
    );
}

/// Writes the job's document to the spool folder, prints it, and removes it.
pub fn print_one(
    job: &PrintJob,
    printer: &dyn Printer,
    spool: Option<&Path>,
    withdrawn: &AtomicBool,
) -> PrintOutcome {
    let spool = spool
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::temp_dir().join("dsa-print"));
    print_spooled(
        job,
        &spool,
        create_private_dir,
        &unique_name("print", ".pdf"),
        withdrawn,
        |document| printer.print(job, document, withdrawn),
    )
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// The desktop's print dialog through the print portal, else CUPS' default printer.
pub struct DesktopPrinter;

impl Printer for DesktopPrinter {
    fn print(&self, job: &PrintJob, document: &Path, withdrawn: &AtomicBool) -> PrintOutcome {
        match portal_print(job, document, withdrawn) {
            Ok(outcome) => outcome,
            Err(PortalError::NoPortal(why)) => {
                eprintln!("print: no print dialog ({why}); printing on the default printer");
                lp_print(job, document)
            }
            Err(PortalError::Failed(why)) => PrintOutcome::Failed(why),
        }
    }
}

enum PortalError {
    /// No portal answered; another way may still print.
    NoPortal(String),
    /// The portal took the job and failed it.
    Failed(String),
}

/// Android's choices as the GTK print settings the portal starts its dialog from.
pub fn portal_settings(settings: &PrintSettings) -> Vec<(&'static str, String)> {
    let mut out = vec![
        ("n-copies", settings.copies.max(1).to_string()),
        ("use-color", settings.color.to_string()),
        (
            "duplex",
            if settings.duplex {
                if settings.landscape {
                    "vertical"
                } else {
                    "horizontal"
                }
            } else {
                "simplex"
            }
            .to_string(),
        ),
        (
            "orientation",
            if settings.landscape {
                "landscape"
            } else {
                "portrait"
            }
            .to_string(),
        ),
    ];
    if settings.media_width_mils > 0 && settings.media_height_mils > 0 {
        out.push((
            "paper-width",
            format!("{:.2}", f64::from(settings.media_width_mils) * 0.0254),
        ));
        out.push((
            "paper-height",
            format!("{:.2}", f64::from(settings.media_height_mils) * 0.0254),
        ));
    }
    out
}

/// Android's choices as `lp` options.
pub fn lp_arguments(job: &PrintJob, document: &Path) -> Vec<String> {
    let s = &job.settings;
    let mut args = vec![
        "-t".to_string(),
        job.title(),
        "-n".to_string(),
        s.copies
            .clamp(1, dsa_host_core::print::MAX_COPIES)
            .to_string(),
        "-o".to_string(),
        format!(
            "sides={}",
            match (s.duplex, s.landscape) {
                (false, _) => "one-sided",
                (true, false) => "two-sided-long-edge",
                (true, true) => "two-sided-short-edge",
            }
        ),
        "-o".to_string(),
        format!(
            "print-color-mode={}",
            if s.color { "color" } else { "monochrome" }
        ),
    ];
    if s.landscape {
        args.extend(["-o".to_string(), "orientation-requested=4".to_string()]);
    }
    if s.media_width_mils > 0 && s.media_height_mils > 0 {
        args.extend([
            "-o".to_string(),
            format!(
                "media=Custom.{:.2}x{:.2}in",
                f64::from(s.media_width_mils) / 1000.0,
                f64::from(s.media_height_mils) / 1000.0
            ),
        ]);
    }
    args.push("--".to_string());
    args.push(document.to_string_lossy().into_owned());
    args
}

fn lp_print(job: &PrintJob, document: &Path) -> PrintOutcome {
    match Command::new("lp")
        .args(lp_arguments(job, document))
        .output()
    {
        Ok(output) if output.status.success() => PrintOutcome::Printed,
        Ok(output) => PrintOutcome::Failed(format!(
            "CUPS did not take the job: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(error) => PrintOutcome::Failed(format!(
            "no print dialog answered, and CUPS' lp cannot run: {error}"
        )),
    }
}

/// The path of the request object the portal makes for `token`, so its answer is watched for
/// before the call that makes it.
pub fn request_path(unique_name: &str, token: &str) -> String {
    let sender = unique_name.trim_start_matches(':').replace('.', "_");
    format!("{PORTAL_PATH}/request/{sender}/{token}")
}

/// Calls `method` on the print portal and waits for its answer: the response code (0 done,
/// 1 canceled, 2 failed) and the results.
fn portal_request(
    connection: &Connection,
    method: &str,
    token: &str,
    call: impl FnOnce(&zbus::blocking::Proxy<'_>) -> zbus::Result<OwnedObjectPath>,
) -> Result<(u32, HashMap<String, OwnedValue>), PortalError> {
    let unique = connection
        .unique_name()
        .map(|n| n.to_string())
        .ok_or_else(|| PortalError::NoPortal("the session bus gave no name".into()))?;
    let request = proxy(connection, PORTAL, request_path(&unique, token), REQUEST)
        .map_err(PortalError::NoPortal)?;
    let mut responses = request
        .receive_signal("Response")
        .map_err(|e| PortalError::NoPortal(e.to_string()))?;
    let print =
        proxy(connection, PORTAL, PORTAL_PATH.into(), PRINT).map_err(PortalError::NoPortal)?;
    call(&print).map_err(|e| PortalError::NoPortal(format!("{method}: {e}")))?;
    let message = responses
        .next()
        .ok_or_else(|| PortalError::Failed("the print dialog went away".into()))?;
    message
        .body()
        .deserialize::<(u32, HashMap<String, OwnedValue>)>()
        .map_err(|e| PortalError::Failed(format!("the print dialog's answer is unreadable: {e}")))
}

fn portal_print(
    job: &PrintJob,
    document: &Path,
    withdrawn: &AtomicBool,
) -> Result<PrintOutcome, PortalError> {
    let connection = Connection::session().map_err(|e| PortalError::NoPortal(e.to_string()))?;
    let title = job.title();
    let token = format!("dsa_print_{}", job.job_id);

    let settings: HashMap<&str, Value<'_>> = portal_settings(&job.settings)
        .into_iter()
        .map(|(key, value)| (key, Value::from(value)))
        .collect();
    let mut page_setup: HashMap<&str, Value<'_>> = HashMap::new();
    let s = &job.settings;
    if s.media_width_mils > 0 && s.media_height_mils > 0 {
        page_setup.insert("Width", Value::from(f64::from(s.media_width_mils) * 0.0254));
        page_setup.insert(
            "Height",
            Value::from(f64::from(s.media_height_mils) * 0.0254),
        );
    }
    page_setup.insert(
        "Orientation",
        Value::from(if s.landscape { "landscape" } else { "portrait" }),
    );
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));

    let (code, mut results) = portal_request(&connection, "PreparePrint", &token, |print| {
        print.call(
            "PreparePrint",
            &("", title.as_str(), settings, page_setup, options),
        )
    })?;
    match code {
        0 => {}
        1 => return Ok(PrintOutcome::Canceled),
        _ => return Err(PortalError::Failed("the print dialog failed".into())),
    }
    if withdrawn.load(Ordering::Relaxed) {
        return Ok(PrintOutcome::Canceled);
    }
    let print_token = results
        .remove("token")
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| PortalError::Failed("the print dialog gave no job token".into()))?;

    let file = File::open(document)
        .map_err(|e| PortalError::Failed(format!("opening {}: {e}", document.display())))?;
    let token = format!("{token}_send");
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("token", Value::from(print_token));
    let (code, _) = portal_request(&connection, "Print", &token, |print| {
        print.call(
            "Print",
            &("", title.as_str(), Fd::from(file.as_fd()), options),
        )
    })?;
    Ok(match code {
        0 => PrintOutcome::Printed,
        1 => PrintOutcome::Canceled,
        _ => PrintOutcome::Failed("the printer did not take the document".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(settings: PrintSettings) -> PrintJob {
        PrintJob {
            job_id: 3,
            package_name: "com.example.docs".into(),
            app_label: "Docs".into(),
            document_name: "Report".into(),
            settings,
            pdf: Arc::new(b"%PDF-1.7".to_vec()),
        }
    }

    #[test]
    fn fr26_androids_choices_reach_cups() {
        let settings = PrintSettings {
            page_count: 2,
            copies: 2,
            color: false,
            duplex: true,
            landscape: false,
            media_width_mils: 8500,
            media_height_mils: 11000,
        };
        let args = lp_arguments(&job(settings), Path::new("/run/user/1000/dsa/print/a.pdf"));
        let line = args.join(" ");
        assert!(line.starts_with("-t Report -n 2 "), "{line}");
        assert!(line.contains("sides=two-sided-long-edge"), "{line}");
        assert!(line.contains("print-color-mode=monochrome"), "{line}");
        assert!(line.contains("media=Custom.8.50x11.00in"), "{line}");
        assert!(
            line.ends_with("-- /run/user/1000/dsa/print/a.pdf"),
            "{line}"
        );
    }

    #[test]
    fn fr26_androids_choices_start_the_print_dialog() {
        let settings = PrintSettings {
            copies: 3,
            color: true,
            landscape: true,
            ..PrintSettings::default()
        };
        let got = portal_settings(&settings);
        assert!(got.contains(&("n-copies", "3".into())));
        assert!(got.contains(&("use-color", "true".into())));
        assert!(got.contains(&("orientation", "landscape".into())));
        assert!(got.contains(&("duplex", "simplex".into())));
    }

    #[test]
    fn fr26_the_portals_request_path_is_known_before_the_call() {
        assert_eq!(
            request_path(":1.42", "dsa_print_3"),
            "/org/freedesktop/portal/desktop/request/1_42/dsa_print_3"
        );
    }

    struct Records(std::sync::Mutex<Vec<u32>>, PrintOutcome);

    impl Printer for Records {
        fn print(&self, job: &PrintJob, document: &Path, _: &AtomicBool) -> PrintOutcome {
            assert_eq!(std::fs::read(document).unwrap(), *job.pdf);
            self.0.lock().unwrap().push(job.job_id);
            self.1.clone()
        }
    }

    #[test]
    fn fr26_a_pdf_from_the_guest_becomes_a_host_print_job_and_its_outcome_returns() {
        let spool = std::env::temp_dir().join(format!("dsa-print-test-{}", std::process::id()));
        let printer = Records(Default::default(), PrintOutcome::Canceled);
        let outcome = print_one(
            &job(PrintSettings::default()),
            &printer,
            Some(&spool),
            &AtomicBool::new(false),
        );
        assert_eq!(outcome, PrintOutcome::Canceled);
        assert_eq!(*printer.0.lock().unwrap(), vec![3]);
        // The document is gone once the job is over.
        assert_eq!(std::fs::read_dir(&spool).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(spool);
    }

    #[test]
    fn fr26_a_job_withdrawn_before_it_prints_is_canceled_unprinted() {
        let spool = std::env::temp_dir().join(format!("dsa-print-wd-{}", std::process::id()));
        let printer = Records(Default::default(), PrintOutcome::Printed);
        let outcome = print_one(
            &job(PrintSettings::default()),
            &printer,
            Some(&spool),
            &AtomicBool::new(true),
        );
        assert_eq!(outcome, PrintOutcome::Canceled);
        assert!(printer.0.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(spool);
    }
}
