//! Printing the documents apps in the user's Android print, on the user's own printers.
//!
//! The window manager watches the print jobs of the user's Android over the local API
//! ([`serve`]). Each job comes whole: a PDF that Android's print dialog laid out, with the
//! settings the user chose there. The window manager writes it to the print spool folder
//! ([`crate::paths::UserRoot::print_spool`]), shows the Windows print dialog so the user picks
//! a printer, pages and copies, and prints ([`WindowsPrinter`]). The file is removed once the
//! job is over, whatever became of it.
//!
//! What became of the job goes back to Android: printed, canceled in the dialog, or failed
//! with the reason, so the app and Android's print queue show the same. A job Android withdraws
//! while its dialog is open (the app canceled it, or Android stopped) is not printed. A job
//! that cannot be printed, for any reason, fails in Android; nothing about a job ends the
//! window manager.
//!
//! The pages are drawn by Windows' own PDF renderer (`Windows.Data.Pdf`) into bitmaps at up to
//! [`RENDER_DPI`], each fitted to the printer's printable area and turned to the paper's
//! orientation, and sent to the printer the dialog returned.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::print::{PrintEvent, PrintJob, PrintOutcome};
use dsa_transport::Connector;

use crate::compositor::log::{note, CompositorLogger};

/// What the window manager calls itself to the service.
const BUILD: &str = "dsa-wm print";

/// How long a request to the service is waited for.
const ANSWER: Duration = Duration::from_secs(5);

/// How long to wait before connecting again after the service could not be reached.
const RETRY: Duration = Duration::from_secs(1);

/// How long the watch waits for the service before it looks for finished jobs again.
const POLL: Duration = Duration::from_millis(100);

/// The highest resolution a page is drawn at, in dots per inch. Text and photos print sharp at
/// it, and a page of A4 or Letter takes about 35 MB while it is drawn.
pub const RENDER_DPI: u32 = 300;

/// Prints one document. The window manager's is the Windows print dialog
/// ([`WindowsPrinter`]); a test's records what it is given.
pub trait Printer: Send + Sync {
    /// Prints `job`, whose PDF is the file `document`, and says what became of it. `withdrawn`
    /// turns true when Android no longer wants it; it is then not printed, or printing stops.
    fn print(&self, job: &PrintJob, document: &Path, withdrawn: &AtomicBool) -> PrintOutcome;
}

/// The print spool folder of the user this process runs as.
pub fn default_spool_dir() -> Option<PathBuf> {
    crate::paths::UserRoot::current().map(|root| root.print_spool())
}

/// Prints the documents of the user's Android until `stop`, reconnecting to the service
/// whenever the connection ends. Each job prints on a thread of its own, so the watch goes on
/// while a print dialog is open; what became of it is passed back through new connections made
/// with `connector`.
pub fn serve(
    mut connector: Box<dyn Connector>,
    printer: Arc<dyn Printer>,
    spool: Option<PathBuf>,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    if let Some(dir) = &spool {
        // Documents an earlier run left: their jobs ended with it.
        for problem in crate::paths::clear_tmp(dir) {
            note(logger.as_deref(), &format!("print spool: {problem}"));
        }
    }
    let (done_tx, done) = mpsc::channel();
    let mut printing: HashMap<u32, Arc<AtomicBool>> = HashMap::new();
    let mut last_error = String::new();
    while !stop.load(Ordering::Relaxed) {
        let stream = HostApiClient::connect(&mut *connector, BUILD, ANSWER)
            .and_then(|client| client.watch_print_jobs());
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                let error = error.to_string();
                if error != last_error {
                    note(
                        logger.as_deref(),
                        &format!("printing cannot reach the service: {error}"),
                    );
                    last_error = error;
                }
                report(&mut *connector, &done, &mut printing, logger.as_deref());
                thread::sleep(RETRY);
                continue;
            }
        };
        last_error.clear();
        while !stop.load(Ordering::Relaxed) {
            report(&mut *connector, &done, &mut printing, logger.as_deref());
            match stream.next(POLL) {
                Ok(Some(PrintEvent::Job(job))) => {
                    let withdrawn = Arc::new(AtomicBool::new(false));
                    printing.insert(job.job_id, Arc::clone(&withdrawn));
                    start_job(
                        job,
                        Arc::clone(&printer),
                        spool.clone(),
                        withdrawn,
                        done_tx.clone(),
                        logger.clone(),
                    );
                }
                Ok(Some(PrintEvent::Withdrawn { job_id })) => {
                    if let Some(withdrawn) = printing.get(&job_id) {
                        withdrawn.store(true, Ordering::Relaxed);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    note(
                        logger.as_deref(),
                        &format!("the print jobs' connection to the service ended: {error}"),
                    );
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
    done: &Receiver<(u32, PrintOutcome)>,
    printing: &mut HashMap<u32, Arc<AtomicBool>>,
    logger: Option<&CompositorLogger>,
) {
    while let Ok((job_id, outcome)) = done.try_recv() {
        printing.remove(&job_id);
        let result = HostApiClient::connect(connector, BUILD, ANSWER)
            .and_then(|mut client| client.report_print_job(job_id, &outcome));
        if let Err(error) = result {
            note(
                logger,
                &format!("what became of print job {job_id} did not reach Android: {error}"),
            );
        }
    }
}

/// Prints one job on a thread of its own, and sends what became of it on `done`.
fn start_job(
    job: PrintJob,
    printer: Arc<dyn Printer>,
    spool: Option<PathBuf>,
    withdrawn: Arc<AtomicBool>,
    done: Sender<(u32, PrintOutcome)>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let job_id = job.job_id;
    let failed = done.clone();
    let spawned = thread::Builder::new()
        .name("dsa-wm-print".into())
        .spawn(move || {
            let outcome = print_one(&job, &*printer, spool.as_deref(), &withdrawn);
            match &outcome {
                PrintOutcome::Failed(why) => note(
                    logger.as_deref(),
                    &format!("{} was not printed: {why}", job.title()),
                ),
                PrintOutcome::Canceled => note(
                    logger.as_deref(),
                    &format!("printing {} was canceled", job.title()),
                ),
                PrintOutcome::Printed => {}
            }
            let _ = done.send((job.job_id, outcome));
        });
    if let Err(error) = spawned {
        let why = format!("the window manager could not start printing: {error}");
        let _ = failed.send((job_id, PrintOutcome::Failed(why)));
    }
}

/// Writes the job's document to the spool folder, prints it, and removes it.
pub fn print_one(
    job: &PrintJob,
    printer: &dyn Printer,
    spool: Option<&Path>,
    withdrawn: &AtomicBool,
) -> PrintOutcome {
    let Some(spool) = spool else {
        return PrintOutcome::Failed("this user has no folder to keep the document in".into());
    };
    if let Err(error) = std::fs::create_dir_all(spool) {
        return PrintOutcome::Failed(format!("creating {}: {error}", spool.display()));
    }
    let document = spool.join(crate::paths::unique_name("print", ".pdf"));
    if let Err(error) = std::fs::write(&document, job.pdf.as_slice()) {
        let _ = std::fs::remove_file(&document);
        return PrintOutcome::Failed(format!("writing {}: {error}", document.display()));
    }
    let outcome = if withdrawn.load(Ordering::Relaxed) {
        PrintOutcome::Canceled
    } else {
        let print = || printer.print(job, &document, withdrawn);
        let printed = catch_unwind(AssertUnwindSafe(print));
        printed.unwrap_or_else(|_| PrintOutcome::Failed("the printing code failed".into()))
    };
    let _ = std::fs::remove_file(&document);
    outcome
}

/// Where a page goes on the paper: the rectangle, in printer pixels, and whether the page is
/// turned a quarter to fit the paper's orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub turned: bool,
}

/// Fits a page of `page_width` by `page_height` inches into a printable area of `area_width`
/// by `area_height` pixels at `dpi_x` by `dpi_y` dots per inch: at its own size when it fits,
/// smaller when it does not, centred, and turned when its orientation is not the paper's.
pub fn place_page(
    page_width: f64,
    page_height: f64,
    area_width: i32,
    area_height: i32,
    dpi_x: i32,
    dpi_y: i32,
) -> Placement {
    let landscape_page = page_width > page_height;
    let landscape_area = area_width > area_height;
    let turned = landscape_page != landscape_area && page_width != page_height;
    let (width, height) = if turned {
        (page_height, page_width)
    } else {
        (page_width, page_height)
    };
    let natural_width = width * f64::from(dpi_x.max(1));
    let natural_height = height * f64::from(dpi_y.max(1));
    let scale = (f64::from(area_width) / natural_width)
        .min(f64::from(area_height) / natural_height)
        .min(1.0);
    let w = ((natural_width * scale).round() as i32).clamp(1, area_width.max(1));
    let h = ((natural_height * scale).round() as i32).clamp(1, area_height.max(1));
    Placement {
        x: (area_width - w) / 2,
        y: (area_height - h) / 2,
        width: w,
        height: h,
        turned,
    }
}

/// The pages to print, numbered from 1, from the ranges the dialog returned: every page when
/// there are none, and only pages the document has.
pub fn pages_to_print(ranges: &[(u32, u32)], page_count: u32) -> Vec<u32> {
    if ranges.is_empty() {
        return (1..=page_count).collect();
    }
    let mut pages = Vec::new();
    for &(from, to) in ranges {
        let (from, to) = (from.min(to), from.max(to));
        for page in from.max(1)..=to.min(page_count) {
            pages.push(page);
        }
    }
    pages
}

/// Turns a picture of `width` by `height` 32-bit pixels a quarter clockwise.
pub fn turn_quarter(pixels: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut turned = vec![0u8; pixels.len()];
    for y in 0..height {
        for x in 0..width {
            let from = (y * width + x) * 4;
            // Row x of the turned picture, column (height - 1 - y); it is `height` wide.
            let to = (x * height + (height - 1 - y)) * 4;
            turned[to..to + 4].copy_from_slice(&pixels[from..from + 4]);
        }
    }
    turned
}

/// Prints through the Windows print dialog and the printer the user picks there.
#[cfg(windows)]
pub struct WindowsPrinter;

#[cfg(windows)]
impl Printer for WindowsPrinter {
    fn print(&self, job: &PrintJob, document: &Path, withdrawn: &AtomicBool) -> PrintOutcome {
        // The dialog and the PDF renderer are COM objects, which need COM on this thread.
        let _com = crate::start_menu::ComApartment::enter();
        match windows_print::print(job, document, withdrawn) {
            Ok(outcome) => outcome,
            Err(why) => PrintOutcome::Failed(why),
        }
    }
}

#[cfg(windows)]
mod windows_print {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    use dsa_host_core::print::{PrintJob, PrintOutcome};
    use windows::core::{Interface, HSTRING};
    use windows::Data::Pdf::{PdfDocument, PdfPage, PdfPageRenderOptions};
    use windows::Storage::StorageFile;
    use windows::Storage::Streams::InMemoryRandomAccessStream;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGR, IWICBitmapSource,
        IWICImagingFactory, IWICPalette, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
        WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{CoCreateInstance, IStream, CLSCTX_INPROC_SERVER};
    use windows::Win32::System::WinRT::CreateStreamOverRandomAccessStream;
    use windows_future::{AsyncStatus, IAsyncInfo};
    use windows_sys::Win32::Foundation::{GlobalFree, HWND};
    use windows_sys::Win32::Graphics::Gdi::{
        DeleteDC, GetDeviceCaps, SetStretchBltMode, StretchDIBits, BITMAPINFO, DIB_RGB_COLORS,
        HALFTONE, HDC, HORZRES, LOGPIXELSX, LOGPIXELSY, SRCCOPY, VERTRES,
    };
    use windows_sys::Win32::Storage::Xps::{
        AbortDoc, EndDoc, EndPage, StartDocW, StartPage, DOCINFOW,
    };
    use windows_sys::Win32::UI::Controls::Dialogs::{
        PrintDlgExW, PD_NOCURRENTPAGE, PD_NOSELECTION, PD_PAGENUMS, PD_RESULT_PRINT,
        PD_RETURNDC, PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGEXW, PRINTPAGERANGE,
        START_PAGE_GENERAL,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, GetSystemMetrics, SetForegroundWindow, ShowWindow,
        SM_CXSCREEN, SM_CYSCREEN, SW_SHOWNA, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };

    use super::{pages_to_print, place_page, turn_quarter, RENDER_DPI};

    /// The most page ranges the dialog takes.
    const MAX_RANGES: usize = 32;

    /// The printer's printable area, in pixels, and its resolution, in dots per inch.
    #[derive(Clone, Copy)]
    struct Area {
        width: i32,
        height: i32,
        dpi_x: i32,
        dpi_y: i32,
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Waits for a WinRT operation to finish.
    fn finish<T: Interface>(operation: &T, what: &str) -> Result<(), String> {
        let info: IAsyncInfo = operation.cast().map_err(|e| format!("{what}: {e}"))?;
        loop {
            let status = info.Status().map_err(|e| format!("{what}: {e}"))?;
            if status == AsyncStatus::Completed {
                return Ok(());
            }
            if status == AsyncStatus::Started {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            if status == AsyncStatus::Canceled {
                return Err(format!("{what}: canceled"));
            }
            let code = info.ErrorCode().map(|code| code.0).unwrap_or_default();
            return Err(format!("{what}: error 0x{code:08x}"));
        }
    }

    /// Opens the document with Windows' PDF renderer.
    fn open(document: &Path) -> Result<PdfDocument, String> {
        let opening = |e: windows::core::Error| format!("opening the document: {e}");
        let reading = |e: windows::core::Error| format!("reading the PDF: {e}");
        let path = HSTRING::from(document.as_os_str());
        let file = StorageFile::GetFileFromPathAsync(&path).map_err(opening)?;
        finish(&file, "opening the document")?;
        let file = file.GetResults().map_err(opening)?;
        let pdf = PdfDocument::LoadFromFileAsync(&file).map_err(reading)?;
        finish(&pdf, "reading the PDF")?;
        pdf.GetResults().map_err(reading)
    }

    /// A page drawn as top-down 32-bit pixels, `width` by `height`.
    struct Picture {
        pixels: Vec<u8>,
        width: u32,
        height: u32,
    }

    /// Draws `page` at `width` by `height` pixels.
    fn render(
        page: &PdfPage,
        width: u32,
        height: u32,
        factory: &IWICImagingFactory,
    ) -> Result<Picture, String> {
        let draw = |e: windows::core::Error| format!("drawing a page: {e}");
        let read = |e: windows::core::Error| format!("reading a drawn page: {e}");
        let options = PdfPageRenderOptions::new().map_err(draw)?;
        options.SetDestinationWidth(width).map_err(draw)?;
        options.SetDestinationHeight(height).map_err(draw)?;
        let stream = InMemoryRandomAccessStream::new().map_err(draw)?;
        let drawing = page
            .RenderWithOptionsToStreamAsync(&stream, &options)
            .map_err(draw)?;
        finish(&drawing, "drawing a page")?;
        drawing.GetResults().map_err(draw)?;
        stream.Seek(0).map_err(read)?;
        let unknown: windows::core::IUnknown = stream.cast().map_err(read)?;
        // SAFETY: COM calls on interfaces this thread made, with buffers that outlive them.
        unsafe {
            let com: IStream = CreateStreamOverRandomAccessStream(&unknown).map_err(read)?;
            let cache = WICDecodeMetadataCacheOnDemand;
            let decoder = factory
                .CreateDecoderFromStream(&com, std::ptr::null(), cache)
                .map_err(read)?;
            let frame = decoder.GetFrame(0).map_err(read)?;
            let converter = factory.CreateFormatConverter().map_err(read)?;
            converter
                .Initialize(
                    &frame,
                    &GUID_WICPixelFormat32bppBGR,
                    WICBitmapDitherTypeNone,
                    None::<&IWICPalette>,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .map_err(read)?;
            let source: IWICBitmapSource = converter.into();
            let (mut w, mut h) = (0u32, 0u32);
            source.GetSize(&mut w, &mut h).map_err(read)?;
            let stride = w * 4;
            let mut pixels = vec![0u8; (stride * h) as usize];
            source
                .CopyPixels(std::ptr::null(), stride, &mut pixels)
                .map_err(read)?;
            Ok(Picture {
                pixels,
                width: w,
                height: h,
            })
        }
    }

    /// An invisible window in the middle of the screen, above the others, that owns the print
    /// dialog, so the dialog comes up in front of the app's window.
    struct Owner(HWND);

    impl Owner {
        fn new() -> Owner {
            let class = wide("STATIC");
            let title = wide("Print");
            // SAFETY: plain calls with strings that outlive them; a null window is handled.
            unsafe {
                let x = GetSystemMetrics(SM_CXSCREEN) / 2;
                let y = GetSystemMetrics(SM_CYSCREEN) / 3;
                let hwnd = CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                    class.as_ptr(),
                    title.as_ptr(),
                    WS_POPUP,
                    x,
                    y,
                    1,
                    1,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                );
                if !hwnd.is_null() {
                    ShowWindow(hwnd, SW_SHOWNA);
                    SetForegroundWindow(hwnd);
                }
                Owner(hwnd)
            }
        }
    }

    impl Drop for Owner {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the window this value made, on this thread.
                unsafe { DestroyWindow(self.0) };
            }
        }
    }

    /// What the user chose in the print dialog: the printer's device context, and the page
    /// ranges (none for every page).
    struct Chosen {
        dc: HDC,
        ranges: Vec<(u32, u32)>,
    }

    impl Drop for Chosen {
        fn drop(&mut self) {
            if !self.dc.is_null() {
                // SAFETY: the printer's device context the dialog made for this job.
                unsafe { DeleteDC(self.dc) };
            }
        }
    }

    /// Shows the print dialog. `None` when the user canceled it.
    fn ask(job: &PrintJob, page_count: u32) -> Result<Option<Chosen>, String> {
        let owner = Owner::new();
        let all = PRINTPAGERANGE {
            nFromPage: 1,
            nToPage: page_count.max(1),
        };
        let mut ranges = [all; MAX_RANGES];
        // SAFETY: a zeroed PRINTDLGEXW is a valid start; every pointer in it is set below to
        // memory that outlives the call, and the handles it returns are freed here or by
        // Chosen.
        unsafe {
            let mut dialog: PRINTDLGEXW = std::mem::zeroed();
            dialog.lStructSize = std::mem::size_of::<PRINTDLGEXW>() as u32;
            dialog.hwndOwner = owner.0;
            dialog.Flags = PD_RETURNDC
                | PD_NOSELECTION
                | PD_NOCURRENTPAGE
                | PD_USEDEVMODECOPIESANDCOLLATE;
            dialog.nPageRanges = 0;
            dialog.nMaxPageRanges = MAX_RANGES as u32;
            dialog.lpPageRanges = ranges.as_mut_ptr();
            dialog.nMinPage = 1;
            dialog.nMaxPage = page_count.max(1);
            dialog.nCopies = job.settings.copies.max(1);
            dialog.nStartPage = START_PAGE_GENERAL;
            let result = PrintDlgExW(&mut dialog);
            if !dialog.hDevMode.is_null() {
                GlobalFree(dialog.hDevMode);
            }
            if !dialog.hDevNames.is_null() {
                GlobalFree(dialog.hDevNames);
            }
            let mut chosen = Chosen {
                dc: dialog.hDC,
                ranges: Vec::new(),
            };
            if result != 0 {
                let code = result as u32;
                return Err(format!("the print dialog did not open (error 0x{code:08x})"));
            }
            if dialog.dwResultAction != PD_RESULT_PRINT {
                return Ok(None);
            }
            if chosen.dc.is_null() {
                return Err("the print dialog gave no printer to print on".into());
            }
            if dialog.Flags & PD_PAGENUMS != 0 {
                let count = (dialog.nPageRanges as usize).min(MAX_RANGES);
                chosen.ranges = ranges[..count]
                    .iter()
                    .map(|range| (range.nFromPage, range.nToPage))
                    .collect();
            }
            Ok(Some(chosen))
        }
    }

    /// Prints `job` from the PDF `document`.
    pub(super) fn print(
        job: &PrintJob,
        document: &Path,
        withdrawn: &AtomicBool,
    ) -> Result<PrintOutcome, String> {
        let pdf = open(document)?;
        let reading = |e: windows::core::Error| format!("reading the PDF: {e}");
        let page_count = pdf.PageCount().map_err(reading)?;
        if page_count == 0 {
            return Err("the document has no pages".into());
        }
        let Some(chosen) = ask(job, page_count)? else {
            return Ok(PrintOutcome::Canceled);
        };
        if withdrawn.load(Ordering::Relaxed) {
            return Ok(PrintOutcome::Canceled);
        }
        let missing = |e: windows::core::Error| format!("the imaging component is missing: {e}");
        // SAFETY: COM is entered on this thread.
        let factory: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .map_err(missing)?;
        let dc = chosen.dc;
        // SAFETY: the dialog's device context, valid until `chosen` is dropped.
        let area = unsafe {
            Area {
                width: GetDeviceCaps(dc, HORZRES),
                height: GetDeviceCaps(dc, VERTRES),
                dpi_x: GetDeviceCaps(dc, LOGPIXELSX),
                dpi_y: GetDeviceCaps(dc, LOGPIXELSY),
            }
        };
        let name = wide(&job.title());
        // SAFETY: a zeroed DOCINFOW with its size and a name that outlives the call.
        let started = unsafe {
            let mut info: DOCINFOW = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<DOCINFOW>() as i32;
            info.lpszDocName = name.as_ptr();
            SetStretchBltMode(dc, HALFTONE);
            StartDocW(dc, &info)
        };
        if started <= 0 {
            return Err("the printer did not take the document".into());
        }
        let pages = pages_to_print(&chosen.ranges, page_count);
        let result = print_pages(&pdf, &pages, dc, area, &factory, withdrawn);
        let printed = matches!(result, Ok(PrintOutcome::Printed));
        // SAFETY: ends the document started above, on the same device context.
        let ended = unsafe {
            if printed {
                EndDoc(dc) > 0
            } else {
                AbortDoc(dc);
                true
            }
        };
        if !ended {
            return Err("the printer did not finish the document".into());
        }
        result
    }

    /// Draws each page and sends it to the printer.
    fn print_pages(
        pdf: &PdfDocument,
        pages: &[u32],
        dc: HDC,
        area: Area,
        factory: &IWICImagingFactory,
        withdrawn: &AtomicBool,
    ) -> Result<PrintOutcome, String> {
        for &number in pages {
            if withdrawn.load(Ordering::Relaxed) {
                return Ok(PrintOutcome::Canceled);
            }
            let page_error = |e: windows::core::Error| format!("reading page {number}: {e}");
            let page = pdf.GetPage(number - 1).map_err(page_error)?;
            let size = page.Size().map_err(page_error)?;
            // A PDF page's size is in device-independent pixels, 96 to an inch.
            let inches_w = f64::from(size.Width) / 96.0;
            let inches_h = f64::from(size.Height) / 96.0;
            let place = place_page(
                inches_w,
                inches_h,
                area.width,
                area.height,
                area.dpi_x,
                area.dpi_y,
            );
            let printer_dpi = area.dpi_x.max(area.dpi_y).max(1) as u32;
            let dpi = f64::from(RENDER_DPI.min(printer_dpi));
            let width = ((inches_w * dpi).round() as u32).clamp(1, 20_000);
            let height = ((inches_h * dpi).round() as u32).clamp(1, 20_000);
            let picture = render(&page, width, height, factory)
                .map_err(|why| format!("page {number}: {why}"))?;
            let (w, h) = (picture.width, picture.height);
            let (pixels, w, h) = if place.turned {
                let turned = turn_quarter(&picture.pixels, w as usize, h as usize);
                (turned, h, w)
            } else {
                (picture.pixels, w, h)
            };
            // SAFETY: a zeroed BITMAPINFO describing `pixels`, top-down 32-bit, which outlives
            // the call; the device context is the printer's, inside a started document.
            unsafe {
                if StartPage(dc) <= 0 {
                    return Err(format!("the printer did not take page {number}"));
                }
                let mut info: BITMAPINFO = std::mem::zeroed();
                info.bmiHeader.biSize = std::mem::size_of_val(&info.bmiHeader) as u32;
                info.bmiHeader.biWidth = w as i32;
                info.bmiHeader.biHeight = -(h as i32);
                info.bmiHeader.biPlanes = 1;
                info.bmiHeader.biBitCount = 32;
                let drawn = StretchDIBits(
                    dc,
                    place.x,
                    place.y,
                    place.width,
                    place.height,
                    0,
                    0,
                    w as i32,
                    h as i32,
                    pixels.as_ptr().cast(),
                    &info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                if drawn == 0 {
                    return Err(format!("page {number} could not be drawn for the printer"));
                }
                if EndPage(dc) <= 0 {
                    return Err(format!("the printer did not take page {number}"));
                }
            }
        }
        Ok(PrintOutcome::Printed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr26_a_page_that_fits_prints_at_its_own_size_in_the_middle() {
        // A5 portrait on a 600 dpi printer's A4 area.
        let place = place_page(5.83, 8.27, 4760, 6779, 600, 600);
        assert!(!place.turned);
        assert_eq!(place.width, 3498);
        assert_eq!(place.height, 4962);
        assert_eq!(place.x, (4760 - 3498) / 2);
    }

    #[test]
    fn fr26_a_landscape_page_is_turned_to_portrait_paper_and_shrunk_to_fit() {
        let place = place_page(11.0, 8.5, 2400, 3150, 300, 300);
        assert!(place.turned);
        assert_eq!(place.width, 2400);
        assert!(place.height <= 3150);
    }

    #[test]
    fn fr26_the_pages_the_user_picked_are_the_ones_printed() {
        assert_eq!(pages_to_print(&[], 3), vec![1, 2, 3]);
        assert_eq!(pages_to_print(&[(2, 3), (5, 9)], 6), vec![2, 3, 5, 6]);
        assert_eq!(pages_to_print(&[(0, 1)], 2), vec![1]);
    }

    #[test]
    fn fr26_turning_a_picture_keeps_every_pixel() {
        let pixels = [1, 1, 1, 1, 2, 2, 2, 2];
        // A row of two turns into a column of two, the first on top.
        assert_eq!(turn_quarter(&pixels, 2, 1), pixels.to_vec());
        // A column of two turns into a row of two, the bottom one first.
        assert_eq!(turn_quarter(&pixels, 1, 2), vec![2, 2, 2, 2, 1, 1, 1, 1]);
    }
}
