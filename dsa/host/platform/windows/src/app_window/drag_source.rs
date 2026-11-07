//! Files and text an app drags out of its task window, as an OLE drag on the host.
//!
//! When a press leaves a task window ([`LeaveWatch`]), the window asks the service, on a thread
//! of its own, whether an app is dragging something out ([`starter`]). If one is, the offer
//! comes back to the window thread as [`WM_APP_DRAG_OUT`], and [`run_pending`] starts
//! `DoDragDrop` with a data object that carries the files as virtual files
//! (`CFSTR_FILEDESCRIPTORW` and `CFSTR_FILECONTENTS`) and the text as `CF_UNICODETEXT`. The
//! files are never written to the host's disk by DSA: the app the user drops them on (Explorer,
//! the desktop, a mail client) reads each one through an `IStream`, whose reads go to the
//! guest in pieces over the control channel. When `DoDragDrop` returns, dropped or cancelled,
//! the drag ends in the guest too, which gives back its access to the app's files.
//!
//! [`LeaveWatch`]: dsa_host_core::drag::LeaveWatch

use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsa_host_core::drag::{DragOffer, DragOut, DragRead, READ_BYTES};
use dsa_host_core::host_api::HostApiClient;
use windows::core::{implement, Ref, BOOL, HRESULT, PCWSTR};
use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC, E_FAIL,
    E_INVALIDARG, E_NOTIMPL, HGLOBAL, OLE_E_ADVISENOTSUPPORTED, STG_E_INVALIDFUNCTION, S_FALSE,
    S_OK,
};
use windows::Win32::System::Com::{
    IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA,
    ISequentialStream_Impl, IStream, IStream_Impl, DATADIR_GET, DVASPECT_CONTENT, FORMATETC,
    LOCKTYPE, STATFLAG, STATSTG, STGC, STGMEDIUM, STGMEDIUM_0, STGTY_STREAM, STREAM_SEEK,
    STREAM_SEEK_CUR, STREAM_SEEK_END, STREAM_SEEK_SET, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY,
    DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::SHCreateStdEnumFmtEtc;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use super::accessibility::ApiConnect;

/// Sent to a task window with a boxed [`PendingDrag`] in `lparam`: an app's drag to start on
/// the host.
pub const WM_APP_DRAG_OUT: u32 = WM_APP + 6;

/// The build the window host names itself by when it drags out of a window.
const BUILD: &str = "dsa-app-window drag";

/// Asks the service whether an app drags out of the window `hwnd` (as an integer) that the
/// press left at `out`.
pub type DragStarter = Arc<dyn Fn(isize, DragOut) + Send + Sync>;

/// What the window thread starts a host drag with: the offer, and the connection that reads
/// its files and ends it.
pub struct PendingDrag {
    offer: DragOffer,
    client: Arc<Mutex<HostApiClient>>,
    note: Arc<dyn Fn(&str) + Send + Sync>,
}

/// The starter of a window host: each question goes to the service at `api` on a thread of
/// its own, one at a time; `note` logs.
pub fn starter(api: ApiConnect, note: Arc<dyn Fn(&str) + Send + Sync>) -> DragStarter {
    let busy = Arc::new(AtomicBool::new(false));
    Arc::new(move |hwnd, out| {
        if busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let (api, worker_note, worker_busy) =
            (Arc::clone(&api), Arc::clone(&note), Arc::clone(&busy));
        let spawned = std::thread::Builder::new()
            .name("dsa-app-window-drag".into())
            .spawn(move || {
                ask(&api, &worker_note, hwnd, out);
                worker_busy.store(false, Ordering::Release);
            });
        if let Err(error) = spawned {
            busy.store(false, Ordering::Release);
            note(&format!("a drag out of the window cannot start: {error}"));
        }
    })
}

/// Asks the service, and hands an offer to the window thread.
fn ask(api: &ApiConnect, note: &Arc<dyn Fn(&str) + Send + Sync>, hwnd: isize, out: DragOut) {
    let mut connector = api();
    let mut client = match HostApiClient::connect(&mut *connector, BUILD, Duration::from_secs(5)) {
        Ok(client) => client,
        Err(error) => {
            note(&format!(
                "a drag out of task {}: the DSA service cannot be reached: {error}",
                out.task_id
            ));
            return;
        }
    };
    let offer = match client.drag_out(out) {
        Ok(Some(offer)) => offer,
        Ok(None) => return,
        Err(error) => {
            note(&format!(
                "a drag out of task {} did not start: {error}",
                out.task_id
            ));
            return;
        }
    };
    note(&format!(
        "task {}: {} item(s) dragged out of the window",
        out.task_id,
        offer.items.len()
    ));
    let drag_id = offer.drag_id;
    let client = Arc::new(Mutex::new(client));
    let pending = Box::into_raw(Box::new(PendingDrag {
        offer,
        client: Arc::clone(&client),
        note: Arc::clone(note),
    }));
    // SAFETY: the window thread takes the box back in run_pending; when the message cannot be
    // posted (the window has closed), it is taken back here.
    if unsafe { PostMessageW(hwnd as _, WM_APP_DRAG_OUT, 0, pending as isize) } == 0 {
        drop(unsafe { Box::from_raw(pending) });
        let _ = client
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .end_drag(drag_id, false);
    }
}

/// Runs the host drag of the [`PendingDrag`] in `lparam`, on the window's thread, until it is
/// dropped or cancelled, then ends it in the guest, and logs how it went.
///
/// # Safety
///
/// `lparam` is the pointer of a [`WM_APP_DRAG_OUT`] message, taken once.
pub unsafe fn run_pending(lparam: isize) {
    let pending = Box::from_raw(lparam as *mut PendingDrag);
    let PendingDrag {
        offer,
        client,
        note,
    } = *pending;
    let drag_id = offer.drag_id;
    // The drag takes the mouse from here; the task's own press ended in the guest already.
    ReleaseCapture();
    let outcome = match DataObject::new(&offer, Arc::clone(&client)) {
        Ok(data) => {
            let data: IDataObject = data.into();
            let source: IDropSource = DropSource.into();
            let mut effect = DROPEFFECT_NONE;
            let result = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect);
            let dropped = result == DRAGDROP_S_DROP && effect != DROPEFFECT_NONE;
            Ok(dropped)
        }
        Err(why) => Err(why),
    };
    let dropped = matches!(outcome, Ok(true));
    let ended = client
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .end_drag(drag_id, dropped);
    note(&match (outcome, ended) {
        (Err(why), _) => format!("the drag out of the window could not start: {why}"),
        (Ok(_), Err(error)) => format!("the drag ended, but Android was not told: {error}"),
        (Ok(true), Ok(())) => "the drag out of the window was dropped".to_string(),
        (Ok(false), Ok(())) => "the drag out of the window was cancelled".to_string(),
    });
}

#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// The registered clipboard format named `name`.
fn registered(name: &str) -> u16 {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: a NUL-terminated name.
    unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) as u16 }
}

fn format(cf: u16, tymed: u32, lindex: i32) -> FORMATETC {
    FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex,
        tymed,
    }
}

/// The data object of one drag: the descriptor of its files, each file's content, its text.
#[implement(IDataObject)]
struct DataObject {
    descriptor_format: u16,
    contents_format: u16,
    /// FILEGROUPDESCRIPTORW, when the drag carries files.
    descriptor: Option<Vec<u8>>,
    /// The drag's item index and size of each file, in descriptor order.
    files: Vec<(u32, Option<u64>)>,
    /// UTF-16 text with its NUL, when the drag carries text.
    text: Option<Vec<u16>>,
    drag_id: u32,
    client: Arc<Mutex<HostApiClient>>,
}

impl DataObject {
    fn new(offer: &DragOffer, client: Arc<Mutex<HostApiClient>>) -> Result<DataObject, String> {
        let files: Vec<_> = offer.files().collect();
        let descriptor = (!files.is_empty()).then(|| {
            file_group_descriptor(
                &files
                    .iter()
                    .map(|(_, item)| (item.name.as_str(), item.size))
                    .collect::<Vec<_>>(),
            )
        });
        let text = offer
            .text()
            .map(|text| text.encode_utf16().chain(std::iter::once(0)).collect());
        if descriptor.is_none() && text.is_none() {
            return Err("the drag carries nothing the host can take".into());
        }
        Ok(DataObject {
            descriptor_format: registered("FileGroupDescriptorW"),
            contents_format: registered("FileContents"),
            descriptor,
            files: files
                .iter()
                .map(|(index, item)| (*index, item.size))
                .collect(),
            text,
            drag_id: offer.drag_id,
            client,
        })
    }

    fn formats(&self) -> Vec<FORMATETC> {
        let mut formats = Vec::new();
        if self.descriptor.is_some() {
            formats.push(format(self.descriptor_format, TYMED_HGLOBAL.0 as u32, -1));
            formats.push(format(self.contents_format, TYMED_ISTREAM.0 as u32, -1));
        }
        if self.text.is_some() {
            formats.push(format(CF_UNICODETEXT.0, TYMED_HGLOBAL.0 as u32, -1));
        }
        formats
    }

    /// Whether `wanted` asks for something this object has.
    fn offers(&self, wanted: &FORMATETC) -> bool {
        if wanted.dwAspect != DVASPECT_CONTENT.0 {
            return false;
        }
        let hglobal = wanted.tymed & TYMED_HGLOBAL.0 as u32 != 0;
        let stream = wanted.tymed & TYMED_ISTREAM.0 as u32 != 0;
        if wanted.cfFormat == self.descriptor_format {
            self.descriptor.is_some() && hglobal
        } else if wanted.cfFormat == self.contents_format {
            stream && wanted.lindex >= 0 && (wanted.lindex as usize) < self.files.len()
        } else if wanted.cfFormat == CF_UNICODETEXT.0 {
            self.text.is_some() && hglobal
        } else {
            false
        }
    }
}

/// `bytes` in a movable global memory block, as a medium.
fn hglobal_medium(bytes: &[u8]) -> windows::core::Result<STGMEDIUM> {
    // SAFETY: a fresh block of the right size, locked while it is written.
    unsafe {
        let global: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        let target = GlobalLock(global) as *mut u8;
        if target.is_null() {
            return Err(E_FAIL.into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
        let _ = GlobalUnlock(global);
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: global },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }
}

impl IDataObject_Impl for DataObject_Impl {
    fn GetData(&self, wanted: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        // SAFETY: OLE passes a valid FORMATETC.
        let wanted = unsafe { wanted.as_ref() }.ok_or(windows::core::Error::from(E_INVALIDARG))?;
        if !self.offers(wanted) {
            return Err(DV_E_FORMATETC.into());
        }
        if wanted.cfFormat == self.descriptor_format {
            return hglobal_medium(self.descriptor.as_deref().unwrap_or_default());
        }
        if wanted.cfFormat == CF_UNICODETEXT.0 {
            let text = self.text.as_deref().unwrap_or_default();
            // SAFETY: u16 is plain data; the slice is viewed as its bytes.
            let bytes =
                unsafe { std::slice::from_raw_parts(text.as_ptr() as *const u8, text.len() * 2) };
            return hglobal_medium(bytes);
        }
        let (item, size) = self.files[wanted.lindex as usize];
        let stream: IStream = GuestFile {
            drag_id: self.drag_id,
            item,
            size,
            position: std::cell::Cell::new(0),
            client: Arc::clone(&self.client),
        }
        .into();
        Ok(STGMEDIUM {
            tymed: TYMED_ISTREAM.0 as u32,
            u: STGMEDIUM_0 {
                pstm: ManuallyDrop::new(Some(stream)),
            },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }

    fn GetDataHere(
        &self,
        _wanted: *const FORMATETC,
        _medium: *mut STGMEDIUM,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, wanted: *const FORMATETC) -> HRESULT {
        // SAFETY: OLE passes a valid FORMATETC.
        match unsafe { wanted.as_ref() } {
            Some(wanted) if self.offers(wanted) => S_OK,
            _ => DV_E_FORMATETC,
        }
    }

    fn GetCanonicalFormatEtc(&self, _in: *const FORMATETC, out: *mut FORMATETC) -> HRESULT {
        if !out.is_null() {
            // SAFETY: OLE passes a FORMATETC to write.
            unsafe { (*out).ptd = std::ptr::null_mut() };
        }
        E_NOTIMPL
    }

    fn SetData(
        &self,
        _format: *const FORMATETC,
        _medium: *const STGMEDIUM,
        _release: BOOL,
    ) -> windows::core::Result<()> {
        // The shell tells a source how the drop went this way; the drag learns it from
        // DoDragDrop instead, and the medium stays the caller's.
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, direction: u32) -> windows::core::Result<IEnumFORMATETC> {
        if direction != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        // SAFETY: a slice of valid formats, which the enumerator copies.
        unsafe { SHCreateStdEnumFmtEtc(&self.formats()) }
    }

    fn DAdvise(
        &self,
        _format: *const FORMATETC,
        _advf: u32,
        _sink: Ref<'_, IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _connection: u32) -> windows::core::Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

/// One dragged file, read from the guest as the drop target reads it.
#[implement(IStream)]
struct GuestFile {
    drag_id: u32,
    item: u32,
    size: Option<u64>,
    position: std::cell::Cell<u64>,
    client: Arc<Mutex<HostApiClient>>,
}

impl ISequentialStream_Impl for GuestFile_Impl {
    fn Read(&self, out: *mut core::ffi::c_void, wanted: u32, read: *mut u32) -> HRESULT {
        if out.is_null() {
            return STG_E_INVALIDFUNCTION;
        }
        let mut filled = 0usize;
        let mut ended = false;
        let mut client = self.client.lock().unwrap_or_else(|e| e.into_inner());
        while filled < wanted as usize && !ended {
            let length = (wanted as usize - filled).min(READ_BYTES as usize) as u32;
            let chunk = match client.read_drag(DragRead {
                drag_id: self.drag_id,
                item: self.item,
                offset: self.position.get(),
                length,
            }) {
                Ok(chunk) => chunk,
                Err(_) => {
                    if !read.is_null() {
                        // SAFETY: the caller's count to write.
                        unsafe { *read = filled as u32 };
                    }
                    return E_FAIL;
                }
            };
            let n = chunk.data.len().min(wanted as usize - filled);
            // SAFETY: `out` holds `wanted` bytes, and `filled + n` stays within them.
            unsafe {
                std::ptr::copy_nonoverlapping(chunk.data.as_ptr(), (out as *mut u8).add(filled), n)
            };
            filled += n;
            self.position.set(self.position.get() + n as u64);
            ended = chunk.end || n == 0;
        }
        if !read.is_null() {
            // SAFETY: the caller's count to write.
            unsafe { *read = filled as u32 };
        }
        if filled < wanted as usize {
            S_FALSE
        } else {
            S_OK
        }
    }

    fn Write(&self, _data: *const core::ffi::c_void, _len: u32, _written: *mut u32) -> HRESULT {
        STG_E_INVALIDFUNCTION
    }
}

impl IStream_Impl for GuestFile_Impl {
    fn Seek(
        &self,
        moved: i64,
        origin: STREAM_SEEK,
        new_position: *mut u64,
    ) -> windows::core::Result<()> {
        let base = if origin == STREAM_SEEK_SET {
            0i128
        } else if origin == STREAM_SEEK_CUR {
            i128::from(self.position.get())
        } else if origin == STREAM_SEEK_END {
            i128::from(
                self.size
                    .ok_or(windows::core::Error::from(STG_E_INVALIDFUNCTION))?,
            )
        } else {
            return Err(STG_E_INVALIDFUNCTION.into());
        };
        let target = base + i128::from(moved);
        if target < 0 {
            return Err(STG_E_INVALIDFUNCTION.into());
        }
        self.position.set(target as u64);
        if !new_position.is_null() {
            // SAFETY: the caller's position to write.
            unsafe { *new_position = target as u64 };
        }
        Ok(())
    }

    fn SetSize(&self, _size: u64) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn CopyTo(
        &self,
        _to: Ref<'_, IStream>,
        _len: u64,
        _read: *mut u64,
        _written: *mut u64,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Commit(&self, _flags: &STGC) -> windows::core::Result<()> {
        Ok(())
    }

    fn Revert(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn LockRegion(&self, _offset: u64, _len: u64, _kind: &LOCKTYPE) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn UnlockRegion(&self, _offset: u64, _len: u64, _kind: u32) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn Stat(&self, stat: *mut STATSTG, _flags: &STATFLAG) -> windows::core::Result<()> {
        if stat.is_null() {
            return Err(E_INVALIDARG.into());
        }
        // SAFETY: the caller's STATSTG to fill; zeroed is a valid STATSTG with no name.
        unsafe {
            *stat = std::mem::zeroed();
            (*stat).r#type = STGTY_STREAM.0 as u32;
            (*stat).cbSize = self.size.unwrap_or(0);
        }
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IStream> {
        Ok(GuestFile {
            drag_id: self.drag_id,
            item: self.item,
            size: self.size,
            position: std::cell::Cell::new(self.position.get()),
            client: Arc::clone(&self.client),
        }
        .into())
    }
}

/// The bytes of one FILEDESCRIPTORW.
const DESCRIPTOR_BYTES: usize = 592;
/// FD_ATTRIBUTES, FD_FILESIZE and FD_PROGRESSUI.
const FD_ATTRIBUTES: u32 = 0x0004;
const FD_FILESIZE: u32 = 0x0040;
const FD_PROGRESSUI: u32 = 0x4000;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
/// Where each field of FILEDESCRIPTORW starts.
const ATTRIBUTES_AT: usize = 36;
const SIZE_HIGH_AT: usize = 64;
const NAME_AT: usize = 72;
/// The longest name FILEDESCRIPTORW holds, its NUL included.
const NAME_CHARS: usize = 260;

/// FILEGROUPDESCRIPTORW for `files` (name and size), as the shell reads it: a count, then one
/// FILEDESCRIPTORW each. A name too long for the descriptor is cut, keeping its extension.
pub fn file_group_descriptor(files: &[(&str, Option<u64>)]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(4 + files.len() * DESCRIPTOR_BYTES);
    bytes.extend_from_slice(&(files.len() as u32).to_le_bytes());
    for (name, size) in files {
        let mut entry = vec![0u8; DESCRIPTOR_BYTES];
        let mut flags = FD_ATTRIBUTES | FD_PROGRESSUI;
        if let Some(size) = size {
            flags |= FD_FILESIZE;
            entry[SIZE_HIGH_AT..SIZE_HIGH_AT + 4]
                .copy_from_slice(&((size >> 32) as u32).to_le_bytes());
            entry[SIZE_HIGH_AT + 4..SIZE_HIGH_AT + 8]
                .copy_from_slice(&(*size as u32).to_le_bytes());
        }
        entry[0..4].copy_from_slice(&flags.to_le_bytes());
        entry[ATTRIBUTES_AT..ATTRIBUTES_AT + 4]
            .copy_from_slice(&FILE_ATTRIBUTE_NORMAL.to_le_bytes());
        for (i, unit) in descriptor_name(name).iter().enumerate() {
            entry[NAME_AT + i * 2..NAME_AT + i * 2 + 2].copy_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&entry);
    }
    bytes
}

/// `name` in UTF-16, at most [`NAME_CHARS`] - 1 units, its extension kept when it is cut.
fn descriptor_name(name: &str) -> Vec<u16> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let limit = NAME_CHARS - 1;
    if units.len() <= limit {
        return units;
    }
    let extension: Vec<u16> = match name.rfind('.') {
        Some(dot) if name.len() - dot <= 16 => name[dot..].encode_utf16().collect(),
        _ => Vec::new(),
    };
    let mut stem: Vec<u16> = name[..name.len() - extension.len().min(name.len())]
        .encode_utf16()
        .take(limit - extension.len())
        .collect();
    // Never end on the first half of a surrogate pair.
    if stem
        .last()
        .is_some_and(|unit| (0xd800..0xdc00).contains(unit))
    {
        stem.pop();
    }
    stem.extend(extension);
    stem
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_of(bytes: &[u8], index: usize) -> String {
        let start = 4 + index * DESCRIPTOR_BYTES + NAME_AT;
        let units: Vec<u16> = bytes[start..start + NAME_CHARS * 2]
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|unit| *unit != 0)
            .collect();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn fr29_explorer_is_told_each_dragged_file_by_name_and_size() {
        let bytes = file_group_descriptor(&[("holiday.jpg", Some(5_000_000_000)), ("notes", None)]);
        assert_eq!(bytes.len(), 4 + 2 * DESCRIPTOR_BYTES);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 2);
        assert_eq!(name_of(&bytes, 0), "holiday.jpg");
        assert_eq!(name_of(&bytes, 1), "notes");
        let first = &bytes[4..4 + DESCRIPTOR_BYTES];
        let flags = u32::from_le_bytes(first[0..4].try_into().unwrap());
        assert_ne!(flags & FD_FILESIZE, 0);
        let high = u32::from_le_bytes(first[SIZE_HIGH_AT..SIZE_HIGH_AT + 4].try_into().unwrap());
        let low = u32::from_le_bytes(
            first[SIZE_HIGH_AT + 4..SIZE_HIGH_AT + 8]
                .try_into()
                .unwrap(),
        );
        assert_eq!((u64::from(high) << 32) | u64::from(low), 5_000_000_000);
        let second = &bytes[4 + DESCRIPTOR_BYTES..];
        assert_eq!(
            u32::from_le_bytes(second[0..4].try_into().unwrap()) & FD_FILESIZE,
            0
        );
    }

    #[test]
    fn fr29_a_long_dragged_name_fits_the_descriptor_and_keeps_its_extension() {
        let long = format!("{}.jpg", "a".repeat(400));
        let bytes = file_group_descriptor(&[(&long, None)]);
        let name = name_of(&bytes, 0);
        assert_eq!(name.len(), NAME_CHARS - 1);
        assert!(name.ends_with(".jpg"));
    }
}
