//! The clipboard the Mac and the guest share, through the general `NSPasteboard`: text, and
//! images as PNG.
//!
//! macOS sends no notice when the pasteboard changes; its change count goes up instead, so
//! the window manager reads [`Pasteboard::changed`] on a timer and passes on what changed.
//! What the host put there itself is not sent back to the guest. An image is read as
//! `public.png`, or as `public.tiff` made into PNG; an image from the guest is written as both,
//! since some programs read only TIFF.

use dsa_host_core::clipboard::{choose, Clip, HostClipboard};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;

use crate::objc::{bytes_of, class, ns_data, ns_string, string_of};

/// `NSPasteboardTypeString`.
const TEXT_TYPE: &str = "public.utf8-plain-text";
/// `NSPasteboardTypePNG`.
const PNG_TYPE: &str = "public.png";
/// `NSPasteboardTypeTIFF`.
const TIFF_TYPE: &str = "public.tiff";
/// `NSBitmapImageFileTypePNG`.
const FILE_TYPE_PNG: usize = 4;

/// The general pasteboard.
pub struct Pasteboard {
    board: Retained<AnyObject>,
    /// The change count of what the host itself last wrote, which is not passed back.
    written: Option<isize>,
    /// The change count last seen.
    seen: isize,
}

// The pasteboard is a process-wide object that may be used from any thread.
unsafe impl Send for Pasteboard {}

impl Pasteboard {
    pub fn general() -> Result<Pasteboard, String> {
        unsafe {
            let board: Option<Retained<AnyObject>> =
                msg_send![class(c"NSPasteboard")?, generalPasteboard];
            let board = board.ok_or("there is no general pasteboard")?;
            let seen: isize = msg_send![&*board, changeCount];
            Ok(Pasteboard {
                board,
                written: None,
                seen,
            })
        }
    }

    /// The pasteboard's change count, which goes up with every change.
    pub fn change_count(&self) -> isize {
        unsafe { msg_send![&*self.board, changeCount] }
    }

    /// The text on the pasteboard, if any.
    pub fn text(&self) -> Option<String> {
        unsafe {
            let text: *mut AnyObject =
                msg_send![&*self.board, stringForType: &*ns_string(TEXT_TYPE)];
            (!text.is_null()).then(|| string_of(text))
        }
    }

    fn data(&self, kind: &str) -> Option<Vec<u8>> {
        unsafe {
            let data: *mut AnyObject = msg_send![&*self.board, dataForType: &*ns_string(kind)];
            (!data.is_null())
                .then(|| bytes_of(data))
                .filter(|bytes| !bytes.is_empty())
        }
    }

    /// The image on the pasteboard as PNG, if any: its PNG, or its TIFF made into PNG.
    pub fn image(&self) -> Option<Vec<u8>> {
        if let Some(png) = self.data(PNG_TYPE) {
            return Some(png);
        }
        let tiff = self.data(TIFF_TYPE)?;
        match convert(&tiff, Some(FILE_TYPE_PNG)) {
            Ok(png) => Some(png),
            Err(why) => {
                eprintln!("clipboard: an image copied on the Mac cannot cross: {why}");
                None
            }
        }
    }

    /// What another program copied since the last call, to pass to the guest. `None` when
    /// nothing changed, the change was the host's own, or the pasteboard holds neither text
    /// nor an image.
    pub fn changed(&mut self) -> Option<Clip> {
        let count = self.change_count();
        if count == self.seen {
            return None;
        }
        self.seen = count;
        if self.written == Some(count) {
            return None;
        }
        self.read()
    }

    /// Puts `text`, which the guest copied, on the pasteboard.
    pub fn set_text(&mut self, text: &str) -> Result<(), String> {
        unsafe {
            let _: isize = msg_send![&*self.board, clearContents];
            let ok: bool = msg_send![
                &*self.board,
                setString: &*ns_string(text),
                forType: &*ns_string(TEXT_TYPE)
            ];
            if !ok {
                return Err("the pasteboard did not take the text".into());
            }
        }
        self.wrote();
        Ok(())
    }

    /// Puts the image `png`, which the guest copied, on the pasteboard as PNG and as TIFF.
    pub fn set_image(&mut self, png: &[u8]) -> Result<(), String> {
        let tiff = convert(png, None).ok();
        unsafe {
            let _: isize = msg_send![&*self.board, clearContents];
            let data = ns_data(png)?;
            let ok: bool = msg_send![&*self.board, setData: &*data, forType: &*ns_string(PNG_TYPE)];
            if !ok {
                return Err("the pasteboard did not take the image".into());
            }
            if let Some(tiff) = tiff {
                let data = ns_data(&tiff)?;
                let _: bool =
                    msg_send![&*self.board, setData: &*data, forType: &*ns_string(TIFF_TYPE)];
            }
        }
        self.wrote();
        Ok(())
    }

    fn wrote(&mut self) {
        let count = self.change_count();
        self.written = Some(count);
        self.seen = count;
    }
}

impl HostClipboard for Pasteboard {
    fn read(&mut self) -> Option<Clip> {
        choose(self.text(), || self.image())
    }

    fn write(&mut self, clip: &Clip) -> Result<(), String> {
        match clip {
            Clip::Text(text) => self.set_text(text),
            Clip::Image(png) => self.set_image(png),
        }
    }
}

/// Reads an image (`bytes`, PNG or TIFF) with `NSBitmapImageRep` and writes it again: as PNG
/// when `png` names the file type, otherwise as TIFF.
fn convert(bytes: &[u8], png: Option<usize>) -> Result<Vec<u8>, String> {
    unsafe {
        let data = ns_data(bytes)?;
        let rep: *mut AnyObject = msg_send![class(c"NSBitmapImageRep")?, imageRepWithData: &*data];
        if rep.is_null() {
            return Err("the image cannot be read".into());
        }
        let out: *mut AnyObject = match png {
            Some(kind) => {
                let properties: *mut AnyObject = msg_send![class(c"NSDictionary")?, dictionary];
                msg_send![rep, representationUsingType: kind, properties: properties]
            }
            None => msg_send![rep, TIFFRepresentation],
        };
        let out = bytes_of(out);
        if out.is_empty() {
            return Err("the image cannot be written".into());
        }
        Ok(out)
    }
}
