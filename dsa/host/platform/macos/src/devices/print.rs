//! Printing a guest document on the Mac: the PDF the guest sends is opened with PDFKit and
//! printed through `NSPrintOperation`, whose print panel lets the user pick the printer and
//! starts from the copies, orientation and paper the guest chose.

use std::sync::mpsc::channel;

use dsa_host_core::print::{PrintJob, PrintOutcome};
use objc2::msg_send;
use objc2::rc::{autoreleasepool, Allocated, Retained};
use objc2::runtime::AnyObject;

use crate::objc::{class, ns_data, ns_string, on_main, CGSize};

#[link(name = "PDFKit", kind = "framework")]
extern "C" {}

/// `kPDFPrintPageScaleToFit`.
const SCALE_TO_FIT: isize = 1;
/// `NSPaperOrientationLandscape`.
const LANDSCAPE: isize = 1;

unsafe fn print_on_main(job: &PrintJob) -> PrintOutcome {
    let run = || -> Result<bool, String> {
        let document_class = class(c"PDFDocument")?;
        let data = ns_data(&job.pdf)?;
        let document: Allocated<AnyObject> = msg_send![document_class, alloc];
        let document: Option<Retained<AnyObject>> = msg_send![document, initWithData: &*data];
        let document = document.ok_or("the document is not a PDF the Mac can read")?;

        let info_class = class(c"NSPrintInfo")?;
        let shared: *mut AnyObject = msg_send![info_class, sharedPrintInfo];
        let info: Option<Retained<AnyObject>> = msg_send![shared, copy];
        let info = info.ok_or("no print settings could be made")?;
        let settings = job.settings;
        if settings.landscape {
            let _: () = msg_send![&*info, setOrientation: LANDSCAPE];
        }
        if settings.media_width_mils > 0 && settings.media_height_mils > 0 {
            // Thousandths of an inch to points.
            let size = CGSize {
                width: f64::from(settings.media_width_mils) * 72.0 / 1000.0,
                height: f64::from(settings.media_height_mils) * 72.0 / 1000.0,
            };
            let _: () = msg_send![&*info, setPaperSize: size];
        }
        let dictionary: *mut AnyObject = msg_send![&*info, dictionary];
        let number_class = class(c"NSNumber")?;
        let copies: *mut AnyObject =
            msg_send![number_class, numberWithInt: settings.copies.max(1) as i32];
        let _: () = msg_send![dictionary, setObject: copies, forKey: &*ns_string("NSCopies")];

        let operation: *mut AnyObject = msg_send![
            &*document,
            printOperationForPrintInfo: &*info,
            scalingMode: SCALE_TO_FIT,
            autoRotate: true
        ];
        if operation.is_null() {
            return Err("the document could not be prepared for printing".into());
        }
        let _: () = msg_send![operation, setJobTitle: &*ns_string(&job.title())];
        let _: () = msg_send![operation, setShowsPrintPanel: true];
        let _: () = msg_send![operation, setShowsProgressPanel: true];
        // The device lender runs in the background; its panel comes to the front.
        let app_class = class(c"NSApplication")?;
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        crate::objc::activate_as_accessory(app);
        let printed: bool = msg_send![operation, runOperation];
        Ok(printed)
    };
    match run() {
        Ok(true) => PrintOutcome::Printed,
        Ok(false) => PrintOutcome::Canceled,
        Err(why) => PrintOutcome::Failed(why),
    }
}

/// Prints `job`, showing the print panel, and waits for the user's answer. Runs the panel on
/// the main thread; call it from any other thread.
pub fn print(job: PrintJob) -> PrintOutcome {
    let (done, outcome) = channel();
    on_main(move || {
        let result = autoreleasepool(|_| unsafe { print_on_main(&job) });
        let _ = done.send(result);
    });
    outcome
        .recv()
        .unwrap_or_else(|_| PrintOutcome::Failed("the print panel closed unexpectedly".into()))
}
