//! The text services framework's side of a task window: the framework's thread manager on the
//! window thread, a document manager for each window, and the text store that gives the
//! framework the window's document ([`super::text_store::TextStore`]).
//!
//! The window is an ordinary text services client: its document manager is associated with
//! the window, so the framework focuses it when the window takes the keyboard focus, and the
//! framework serves every input method to it (the Microsoft Korean IME, the touch keyboard,
//! dictation with Win+H, handwriting, the emoji panel). The framework asks for a lock, reads
//! and edits the document under it, and the store sends the guest what each edit changed once
//! the lock is let go. It places the candidate list by the guest's cursor (`GetTextExt`), and
//! reads the editor's input scope as an attribute of the document.
//!
//! The framework's calls come on the window thread and may come back into the store while it
//! notifies the framework, so no borrow of the store's state is held across a call into the
//! framework.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use dsa_host_core::ime::{ImeCursor, ImeEditor, ImeEvent, InputKind};
use dsa_host_core::input::InputEvent;
use windows::core::{implement, IUnknown, Interface, Ref, BOOL, GUID, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{E_INVALIDARG, E_NOTIMPL, E_UNEXPECTED, HWND, POINT, RECT, S_OK};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemAlloc, IDataObject, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, FORMATETC,
};
use windows::Win32::System::Ole::{CONNECT_E_ADVISELIMIT, CONNECT_E_NOCONNECTION};
use windows::Win32::System::Variant::{VARIANT, VT_UNKNOWN};
use windows::Win32::UI::TextServices::{
    CLSID_TF_ThreadMgr, ITextStoreACP, ITextStoreACPSink, ITextStoreACP_Impl, ITfCompartmentMgr,
    ITfCompositionView, ITfContext, ITfContextOwnerCompositionServices,
    ITfContextOwnerCompositionSink, ITfContextOwnerCompositionSink_Impl, ITfDocumentMgr,
    ITfInputScope, ITfInputScope_Impl, ITfRange, ITfRangeACP, ITfThreadMgr, InputScope,
    GUID_COMPARTMENT_KEYBOARD_DISABLED, GUID_PROP_INPUTSCOPE, TEXT_STORE_LOCK_FLAGS,
    TEXT_STORE_TEXT_CHANGE_FLAGS, TF_POPF_ALL, TS_AE_END, TS_AE_START, TS_AS_LAYOUT_CHANGE,
    TS_AS_SEL_CHANGE, TS_AS_TEXT_CHANGE, TS_ATTRVAL, TS_DEFAULT_SELECTION, TS_E_INVALIDPOS,
    TS_E_NOLAYOUT, TS_E_NOLOCK, TS_E_SYNCHRONOUS, TS_IAS_QUERYONLY, TS_LC_CHANGE, TS_LF_READ,
    TS_LF_READWRITE, TS_LF_SYNC, TS_RT_PLAIN, TS_RUNINFO, TS_SELECTIONSTYLE, TS_SELECTION_ACP,
    TS_SS_NOHIDDENTEXT, TS_SS_TKBAUTOCORRECTENABLE, TS_SS_TKBPREDICTIONENABLE, TS_STATUS,
    TS_S_ASYNC, TS_TEXTCHANGE,
};

use super::text_store::{input_scopes, Described, TextChange, TextStore};

/// The one view of the document the store has.
const VIEW: u32 = 1;

/// The write half of a lock's flags: a read-write lock without its read flag.
const WRITE: u32 = TS_LF_READWRITE.0 & !TS_LF_READ.0;

/// The framework on this window thread.
struct Framework {
    manager: ITfThreadMgr,
    client: u32,
}

thread_local! {
    static FRAMEWORK: RefCell<Option<Framework>> = const { RefCell::new(None) };
}

/// Starts the text services framework on the window thread, before its first window opens.
/// Without it the windows still take every character the keys type, but no input method
/// composes in them.
pub fn start_thread() -> Result<(), String> {
    // SAFETY: COM and the framework, on this thread, which keeps them for its lifetime.
    unsafe {
        let joined = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if joined.is_err() {
            return Err(format!(
                "the window thread cannot use COM in a single-threaded apartment ({joined}), so \
                 no input method composes in its windows"
            ));
        }
        let manager: ITfThreadMgr = CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER)
            .map_err(|why| {
                format!("the text services framework is unavailable ({why}); no input method composes in the windows")
            })?;
        let client = manager.Activate().map_err(|why| {
            format!("the text services framework did not start ({why}); no input method composes in the windows")
        })?;
        FRAMEWORK.with(|framework| *framework.borrow_mut() = Some(Framework { manager, client }));
    }
    Ok(())
}

/// Stops the framework on the window thread, once its windows have closed.
pub fn end_thread() {
    if let Some(framework) = FRAMEWORK.with(|framework| framework.borrow_mut().take()) {
        // SAFETY: the manager this thread activated.
        let _ = unsafe { framework.manager.Deactivate() };
    }
}

/// The framework's sink for the store, which it is told of changes through.
#[derive(Clone)]
struct Sink {
    sink: ITextStoreACPSink,
    identity: IUnknown,
    mask: u32,
}

/// Where the input method's windows go on the screen: beside `cursor`, which the window
/// manager gives in the window's client pixels (the task's at the scale the window shows the
/// task at), in the client area `client` on the screen, whose corner may be at negative
/// coordinates on a monitor left of or above the main one. A cursor of no width is one pixel
/// wide, so the candidate list has something to stand beside.
fn cursor_on_screen(client: RECT, cursor: ImeCursor) -> RECT {
    RECT {
        left: client.left + cursor.left,
        top: client.top + cursor.top,
        right: client.left + cursor.right.max(cursor.left + 1),
        bottom: client.top + cursor.bottom,
    }
}

/// One window's text input, shared by the window and the text store the framework holds.
pub struct Shared {
    hwnd: Cell<isize>,
    store: RefCell<TextStore>,
    events: Sender<InputEvent>,
    sink: RefCell<Option<Sink>>,
    /// The lock the framework holds now: 0, or TS_LF_READ or TS_LF_READWRITE.
    lock: Cell<u32>,
    /// A lock the framework asked for while it held one, granted once that one is let go.
    queued: Cell<u32>,
    /// The framework's composition, while it composes.
    composition: RefCell<Option<ITfCompositionView>>,
    /// Where the guest last said the cursor of the task's focused editor is.
    cursor: Cell<Option<ImeCursor>>,
    /// Whether the framework asked for the input scope, which it then retrieves.
    scope_asked: Cell<bool>,
    /// A description of the editor that came while the framework held a lock.
    pending: RefCell<Option<ImeEditor>>,
    /// What a description taken after a lock asks of the document: a new context, the end
    /// of the composition.
    owed: Cell<Described>,
}

impl Shared {
    pub fn new(task_id: i32, events: Sender<InputEvent>) -> Rc<Shared> {
        Rc::new(Shared {
            hwnd: Cell::new(0),
            store: RefCell::new(TextStore::new(task_id)),
            events,
            sink: RefCell::new(None),
            lock: Cell::new(0),
            queued: Cell::new(0),
            composition: RefCell::new(None),
            cursor: Cell::new(None),
            scope_asked: Cell::new(false),
            pending: RefCell::new(None),
            owed: Cell::new(Described::default()),
        })
    }

    pub fn cursor(&self) -> Option<ImeCursor> {
        self.cursor.get()
    }

    /// What the guest last said of its editor.
    pub fn described(&self) -> Option<ImeEditor> {
        self.store
            .try_borrow()
            .ok()
            .and_then(|store| store.described().cloned())
    }

    pub fn is_composing(&self) -> bool {
        self.composition
            .try_borrow()
            .is_ok_and(|composition| composition.is_some())
    }

    fn send(&self, events: Vec<ImeEvent>) {
        for event in events {
            // The sender ends only when the window host stops; input then has nowhere to go.
            let _ = self.events.send(InputEvent::Ime(event));
        }
    }

    fn sink(&self) -> Option<Sink> {
        self.sink.try_borrow().ok().and_then(|sink| sink.clone())
    }

    fn locked(&self, write: bool) -> bool {
        let lock = self.lock.get();
        if write {
            lock & WRITE != 0
        } else {
            lock != 0
        }
    }

    /// Tells the framework the document's text changed, when it listens for that.
    fn tell_text(&self, change: TextChange) {
        let Some(sink) = self.sink() else { return };
        if sink.mask & TS_AS_TEXT_CHANGE == 0 {
            return;
        }
        let change = TS_TEXTCHANGE {
            acpStart: change.start as i32,
            acpOldEnd: change.old_end as i32,
            acpNewEnd: change.new_end as i32,
        };
        // SAFETY: the sink the framework advised, on its thread, with no lock held.
        let _ = unsafe {
            sink.sink
                .OnTextChange(TEXT_STORE_TEXT_CHANGE_FLAGS(0), &change)
        };
    }

    fn tell_selection(&self) {
        let Some(sink) = self.sink() else { return };
        if sink.mask & TS_AS_SEL_CHANGE != 0 {
            // SAFETY: as above.
            let _ = unsafe { sink.sink.OnSelectionChange() };
        }
    }

    fn tell_layout(&self) {
        let Some(sink) = self.sink() else { return };
        if sink.mask & TS_AS_LAYOUT_CHANGE != 0 {
            // SAFETY: as above.
            let _ = unsafe { sink.sink.OnLayoutChange(TS_LC_CHANGE, VIEW) };
        }
    }

    /// Grants the framework a lock and lets it go once the framework is done with it.
    fn grant(&self, sink: &Sink, flags: u32) -> HRESULT {
        self.lock.set(flags);
        // SAFETY: the sink the framework advised, on its thread.
        let granted = unsafe { sink.sink.OnLockGranted(TEXT_STORE_LOCK_FLAGS(flags)) };
        self.lock.set(0);
        match granted {
            Ok(()) => S_OK,
            Err(error) => error.code(),
        }
    }

    /// After the framework let its lock go: the composition as it stands, what changed for
    /// the guest, and a description that waited for the lock.
    fn edited(&self) {
        let extent = self
            .composition
            .try_borrow()
            .ok()
            .and_then(|view| view.as_ref().and_then(composition_extent));
        let events = match self.store.try_borrow_mut() {
            Ok(mut store) => {
                if let Some(extent) = extent {
                    store.set_composition(Some(extent));
                }
                store.flush()
            }
            Err(_) => Vec::new(),
        };
        self.send(events);
        let trimmed = self
            .store
            .try_borrow_mut()
            .ok()
            .and_then(|mut store| store.trim());
        if let Some(change) = trimmed {
            self.tell_text(change);
            self.tell_selection();
        }
    }

    /// A key typed `unit` without the input method.
    pub fn typed(&self, unit: u16) {
        let typed = self
            .store
            .try_borrow_mut()
            .ok()
            .map(|mut store| store.typed(unit));
        let Some((events, change)) = typed else {
            return;
        };
        self.send(events);
        if let (Some(change), false) = (change, self.locked(false)) {
            self.tell_text(change);
            self.tell_selection();
        }
    }

    /// The guest said where the cursor of the task's focused editor is: the input method's
    /// windows go beside it from now on.
    pub fn cursor_moved(&self, cursor: ImeCursor) {
        self.cursor.set(Some(cursor));
        if !self.locked(false) {
            self.tell_layout();
        }
    }

    /// The guest described its editor. `None` while the framework holds a lock: the
    /// description waits for it to be let go.
    fn describe(&self, editor: ImeEditor) -> Option<Described> {
        if self.locked(false) {
            if let Ok(mut pending) = self.pending.try_borrow_mut() {
                *pending = Some(editor);
            }
            return None;
        }
        let told = self.store.try_borrow_mut().ok()?.described_as(editor);
        if let Some(change) = told.text {
            self.tell_text(change);
        }
        if told.selection || told.text.is_some() {
            self.tell_selection();
        }
        Some(told)
    }

    /// The guest described its editor while the window has no framework: the store keeps it,
    /// so typed characters still follow the editor's kind.
    pub fn described_without_framework(&self, editor: ImeEditor) {
        if let Ok(mut store) = self.store.try_borrow_mut() {
            store.described_as(editor);
        }
    }

    fn take_pending(&self) -> Option<ImeEditor> {
        self.pending.try_borrow_mut().ok()?.take()
    }

    fn kind(&self) -> InputKind {
        self.store
            .try_borrow()
            .map(|store| store.kind())
            .unwrap_or_default()
    }

    /// The window's client area, in screen pixels.
    fn client_on_screen(&self) -> Option<RECT> {
        let hwnd = self.hwnd.get() as windows_sys::Win32::Foundation::HWND;
        let mut client = windows_sys::Win32::Foundation::RECT::default();
        // SAFETY: plain Win32 calls on the window of this thread.
        unsafe {
            if windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client) == 0 {
                return None;
            }
            let mut origin = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
            windows_sys::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut origin);
            Some(RECT {
                left: client.left + origin.x,
                top: client.top + origin.y,
                right: client.right + origin.x,
                bottom: client.bottom + origin.y,
            })
        }
    }
}

/// Where a composition runs in the document.
fn composition_extent(view: &ITfCompositionView) -> Option<(u32, u32)> {
    // SAFETY: reading the composition's range, on the framework's thread.
    let range = unsafe { view.GetRange() }.ok()?;
    range_extent(&range)
}

fn range_extent(range: &ITfRange) -> Option<(u32, u32)> {
    let range: ITfRangeACP = range.cast().ok()?;
    let (mut start, mut length) = (0i32, 0i32);
    // SAFETY: as above.
    unsafe { range.GetExtent(&mut start, &mut length) }.ok()?;
    let start = u32::try_from(start).ok()?;
    let length = u32::try_from(length).ok()?;
    Some((start, start + length))
}

fn error(code: HRESULT) -> windows::core::Error {
    windows::core::Error::from(code)
}

/// The text store the framework holds for one window's context.
#[implement(ITextStoreACP, ITfContextOwnerCompositionSink)]
struct Store {
    shared: Rc<Shared>,
}

impl Store_Impl {
    fn need(&self, write: bool) -> windows::core::Result<()> {
        if self.shared.locked(write) {
            Ok(())
        } else {
            Err(error(TS_E_NOLOCK))
        }
    }
}

impl ITextStoreACP_Impl for Store_Impl {
    fn AdviseSink(
        &self,
        riid: *const GUID,
        punk: Ref<'_, IUnknown>,
        dwmask: u32,
    ) -> windows::core::Result<()> {
        let punk = punk.ok()?;
        // SAFETY: the framework passes a valid interface id.
        if riid.is_null() || unsafe { *riid } != ITextStoreACPSink::IID {
            return Err(error(E_INVALIDARG));
        }
        let identity: IUnknown = punk.cast()?;
        let mut sink = self
            .shared
            .sink
            .try_borrow_mut()
            .map_err(|_| error(E_UNEXPECTED))?;
        if let Some(existing) = sink.as_mut() {
            if existing.identity == identity {
                existing.mask = dwmask;
                return Ok(());
            }
            return Err(error(CONNECT_E_ADVISELIMIT));
        }
        *sink = Some(Sink {
            sink: punk.cast()?,
            identity,
            mask: dwmask,
        });
        Ok(())
    }

    fn UnadviseSink(&self, punk: Ref<'_, IUnknown>) -> windows::core::Result<()> {
        let identity: IUnknown = punk.ok()?.cast()?;
        let mut sink = self
            .shared
            .sink
            .try_borrow_mut()
            .map_err(|_| error(E_UNEXPECTED))?;
        if sink.as_ref().is_some_and(|sink| sink.identity == identity) {
            *sink = None;
            Ok(())
        } else {
            Err(error(CONNECT_E_NOCONNECTION))
        }
    }

    fn RequestLock(&self, dwlockflags: u32) -> windows::core::Result<HRESULT> {
        let Some(sink) = self.shared.sink() else {
            return Err(error(E_UNEXPECTED));
        };
        let wanted = dwlockflags & TS_LF_READWRITE.0;
        if self.shared.lock.get() != 0 {
            if dwlockflags & TS_LF_SYNC != 0 {
                return Ok(TS_E_SYNCHRONOUS);
            }
            self.shared.queued.set(self.shared.queued.get() | wanted);
            return Ok(TS_S_ASYNC);
        }
        let granted = self.shared.grant(&sink, wanted);
        loop {
            let queued = self.shared.queued.replace(0);
            if queued == 0 {
                break;
            }
            // The framework asked for this lock earlier and was told TS_S_ASYNC; the sink's
            // result goes to the sink, and nobody waits for it here.
            let _ = self.shared.grant(&sink, queued);
        }
        self.shared.edited();
        if let Some(editor) = self.shared.take_pending() {
            // The window acts on what it asks for at its next description.
            if let Some(told) = self.shared.describe(editor) {
                let owed = self.shared.owed.get();
                self.shared.owed.set(Described {
                    kind: owed.kind || told.kind,
                    ends_composition: owed.ends_composition || told.ends_composition,
                    ..owed
                });
            }
        }
        Ok(granted)
    }

    fn GetStatus(&self) -> windows::core::Result<TS_STATUS> {
        let secret = self.shared.kind().is_secret();
        Ok(TS_STATUS {
            dwDynamicFlags: 0,
            dwStaticFlags: if secret {
                TS_SS_NOHIDDENTEXT
            } else {
                TS_SS_NOHIDDENTEXT | TS_SS_TKBAUTOCORRECTENABLE | TS_SS_TKBPREDICTIONENABLE
            },
        })
    }

    fn QueryInsert(
        &self,
        acpteststart: i32,
        acptestend: i32,
        _cch: u32,
        pacpresultstart: *mut i32,
        pacpresultend: *mut i32,
    ) -> windows::core::Result<()> {
        let len = self
            .shared
            .store
            .try_borrow()
            .map_err(|_| error(E_UNEXPECTED))?
            .len() as i32;
        if acpteststart < 0 || acptestend < acpteststart || acptestend > len {
            return Err(error(E_INVALIDARG));
        }
        if pacpresultstart.is_null() || pacpresultend.is_null() {
            return Err(error(E_INVALIDARG));
        }
        // SAFETY: the framework's out pointers, checked above.
        unsafe {
            *pacpresultstart = acpteststart;
            *pacpresultend = acptestend;
        }
        Ok(())
    }

    fn GetSelection(
        &self,
        ulindex: u32,
        ulcount: u32,
        pselection: *mut TS_SELECTION_ACP,
        pcfetched: *mut u32,
    ) -> windows::core::Result<()> {
        self.need(false)?;
        if pcfetched.is_null() || (ulcount > 0 && pselection.is_null()) {
            return Err(error(E_INVALIDARG));
        }
        if ulindex != 0 && ulindex != TS_DEFAULT_SELECTION {
            return Err(error(E_INVALIDARG));
        }
        let (start, end, backwards) = self
            .shared
            .store
            .try_borrow()
            .map_err(|_| error(E_UNEXPECTED))?
            .selection();
        // SAFETY: the framework's out pointers, checked above.
        unsafe {
            if ulcount == 0 {
                *pcfetched = 0;
                return Ok(());
            }
            *pselection = TS_SELECTION_ACP {
                acpStart: start as i32,
                acpEnd: end as i32,
                style: TS_SELECTIONSTYLE {
                    ase: if backwards { TS_AE_START } else { TS_AE_END },
                    fInterimChar: false.into(),
                },
            };
            *pcfetched = 1;
        }
        Ok(())
    }

    fn SetSelection(
        &self,
        ulcount: u32,
        pselection: *const TS_SELECTION_ACP,
    ) -> windows::core::Result<()> {
        self.need(true)?;
        if ulcount == 0 || pselection.is_null() {
            return Err(error(E_INVALIDARG));
        }
        // SAFETY: the framework's selection, checked above.
        let selection = unsafe { *pselection };
        if selection.acpStart < 0 || selection.acpEnd < 0 {
            return Err(error(TS_E_INVALIDPOS));
        }
        self.shared
            .store
            .try_borrow_mut()
            .map_err(|_| error(E_UNEXPECTED))?
            .set_selection(
                selection.acpStart as u32,
                selection.acpEnd as u32,
                selection.style.ase == TS_AE_START,
            )
            .map_err(|_| error(TS_E_INVALIDPOS))
    }

    fn GetText(
        &self,
        acpstart: i32,
        acpend: i32,
        pchplain: PWSTR,
        cchplainreq: u32,
        pcchplainret: *mut u32,
        prgruninfo: *mut TS_RUNINFO,
        cruninforeq: u32,
        pcruninforet: *mut u32,
        pacpnext: *mut i32,
    ) -> windows::core::Result<()> {
        self.need(false)?;
        let store = self
            .shared
            .store
            .try_borrow()
            .map_err(|_| error(E_UNEXPECTED))?;
        let len = store.len() as i32;
        let end = if acpend == -1 { len } else { acpend };
        if acpstart < 0 || acpstart > len || end < acpstart || end > len {
            return Err(error(TS_E_INVALIDPOS));
        }
        let text = store.text(acpstart as u32, end as u32);
        let count = if pchplain.is_null() {
            0
        } else {
            text.len().min(cchplainreq as usize)
        };
        // SAFETY: the framework's buffers, of the sizes it gives, and its out pointers.
        unsafe {
            if count > 0 {
                std::ptr::copy_nonoverlapping(text.as_ptr(), pchplain.0, count);
            }
            if !pcchplainret.is_null() {
                *pcchplainret = count as u32;
            }
            let runs = if cruninforeq > 0 && !prgruninfo.is_null() && count > 0 {
                *prgruninfo = TS_RUNINFO {
                    uCount: count as u32,
                    r#type: TS_RT_PLAIN,
                };
                1
            } else {
                0
            };
            if !pcruninforet.is_null() {
                *pcruninforet = runs;
            }
            if !pacpnext.is_null() {
                *pacpnext = acpstart + count as i32;
            }
        }
        Ok(())
    }

    fn SetText(
        &self,
        _dwflags: u32,
        acpstart: i32,
        acpend: i32,
        pchtext: &PCWSTR,
        cch: u32,
    ) -> windows::core::Result<TS_TEXTCHANGE> {
        self.need(true)?;
        let text = wide_slice(pchtext, cch)?;
        if acpstart < 0 || acpend < 0 {
            return Err(error(TS_E_INVALIDPOS));
        }
        let change = self
            .shared
            .store
            .try_borrow_mut()
            .map_err(|_| error(E_UNEXPECTED))?
            .replace(acpstart as u32, acpend as u32, text)
            .map_err(|_| error(TS_E_INVALIDPOS))?;
        Ok(text_change(change))
    }

    fn GetFormattedText(&self, _acpstart: i32, _acpend: i32) -> windows::core::Result<IDataObject> {
        Err(error(E_NOTIMPL))
    }

    fn GetEmbedded(
        &self,
        _acppos: i32,
        _rguidservice: *const GUID,
        _riid: *const GUID,
    ) -> windows::core::Result<IUnknown> {
        Err(error(E_NOTIMPL))
    }

    fn QueryInsertEmbedded(
        &self,
        _pguidservice: *const GUID,
        _pformatetc: *const FORMATETC,
    ) -> windows::core::Result<BOOL> {
        // The guest's editor takes text only.
        Ok(false.into())
    }

    fn InsertEmbedded(
        &self,
        _dwflags: u32,
        _acpstart: i32,
        _acpend: i32,
        _pdataobject: Ref<'_, IDataObject>,
    ) -> windows::core::Result<TS_TEXTCHANGE> {
        Err(error(E_NOTIMPL))
    }

    fn InsertTextAtSelection(
        &self,
        dwflags: u32,
        pchtext: &PCWSTR,
        cch: u32,
        pacpstart: *mut i32,
        pacpend: *mut i32,
        pchange: *mut TS_TEXTCHANGE,
    ) -> windows::core::Result<()> {
        let query_only = dwflags & TS_IAS_QUERYONLY != 0;
        self.need(!query_only)?;
        let mut store = self
            .shared
            .store
            .try_borrow_mut()
            .map_err(|_| error(E_UNEXPECTED))?;
        let (start, end) = if query_only {
            let (start, end, _) = store.selection();
            (start, end)
        } else {
            let text = wide_slice(pchtext, cch)?;
            let change = store.insert_at_selection(text);
            if !pchange.is_null() {
                // SAFETY: the framework's out pointer.
                unsafe { *pchange = text_change(change) };
            }
            (change.start, change.new_end)
        };
        // SAFETY: the framework's out pointers, which it may leave null.
        unsafe {
            if !pacpstart.is_null() {
                *pacpstart = start as i32;
            }
            if !pacpend.is_null() {
                *pacpend = end as i32;
            }
        }
        Ok(())
    }

    fn InsertEmbeddedAtSelection(
        &self,
        _dwflags: u32,
        _pdataobject: Ref<'_, IDataObject>,
        _pacpstart: *mut i32,
        _pacpend: *mut i32,
        _pchange: *mut TS_TEXTCHANGE,
    ) -> windows::core::Result<()> {
        Err(error(E_NOTIMPL))
    }

    fn RequestSupportedAttrs(
        &self,
        _dwflags: u32,
        cfilterattrs: u32,
        pafilterattrs: *const GUID,
    ) -> windows::core::Result<()> {
        self.ask_attributes(cfilterattrs, pafilterattrs);
        Ok(())
    }

    fn RequestAttrsAtPosition(
        &self,
        _acppos: i32,
        cfilterattrs: u32,
        pafilterattrs: *const GUID,
        _dwflags: u32,
    ) -> windows::core::Result<()> {
        self.ask_attributes(cfilterattrs, pafilterattrs);
        Ok(())
    }

    fn RequestAttrsTransitioningAtPosition(
        &self,
        _acppos: i32,
        _cfilterattrs: u32,
        _pafilterattrs: *const GUID,
        _dwflags: u32,
    ) -> windows::core::Result<()> {
        // The input scope holds for the whole document, so nothing changes at a position.
        Ok(())
    }

    fn FindNextAttrTransition(
        &self,
        _acpstart: i32,
        acphalt: i32,
        _cfilterattrs: u32,
        _pafilterattrs: *const GUID,
        _dwflags: u32,
        pacpnext: *mut i32,
        pffound: *mut BOOL,
        plfoundoffset: *mut i32,
    ) -> windows::core::Result<()> {
        // SAFETY: the framework's out pointers.
        unsafe {
            if !pacpnext.is_null() {
                *pacpnext = acphalt;
            }
            if !pffound.is_null() {
                *pffound = false.into();
            }
            if !plfoundoffset.is_null() {
                *plfoundoffset = 0;
            }
        }
        Ok(())
    }

    fn RetrieveRequestedAttrs(
        &self,
        ulcount: u32,
        paattrvals: *mut TS_ATTRVAL,
        pcfetched: *mut u32,
    ) -> windows::core::Result<()> {
        if pcfetched.is_null() {
            return Err(error(E_INVALIDARG));
        }
        let asked = self.shared.scope_asked.replace(false);
        // SAFETY: the framework's buffer of `ulcount` values and its out pointer.
        unsafe {
            if !asked || ulcount == 0 || paattrvals.is_null() {
                *pcfetched = 0;
                return Ok(());
            }
            let scopes: ITfInputScope = Scopes {
                scopes: input_scopes(self.shared.kind()),
            }
            .into();
            let mut value = VARIANT::default();
            let inner = &mut *value.Anonymous.Anonymous;
            inner.vt = VT_UNKNOWN;
            // The framework owns the value it is handed, and releases it.
            inner.Anonymous.punkVal = std::mem::ManuallyDrop::new(Some(scopes.cast()?));
            std::ptr::write(
                paattrvals,
                TS_ATTRVAL {
                    idAttr: GUID_PROP_INPUTSCOPE,
                    dwOverlapId: 0,
                    varValue: value,
                },
            );
            *pcfetched = 1;
        }
        Ok(())
    }

    fn GetEndACP(&self) -> windows::core::Result<i32> {
        self.need(false)?;
        Ok(self
            .shared
            .store
            .try_borrow()
            .map_err(|_| error(E_UNEXPECTED))?
            .len() as i32)
    }

    fn GetActiveView(&self) -> windows::core::Result<u32> {
        Ok(VIEW)
    }

    fn GetACPFromPoint(
        &self,
        _vcview: u32,
        _ptscreen: *const POINT,
        _dwflags: u32,
    ) -> windows::core::Result<i32> {
        // The host knows where the guest's cursor is, not where each character is.
        Err(error(E_NOTIMPL))
    }

    fn GetTextExt(
        &self,
        _vcview: u32,
        _acpstart: i32,
        _acpend: i32,
        prc: *mut RECT,
        pfclipped: *mut BOOL,
    ) -> windows::core::Result<()> {
        self.need(false)?;
        if prc.is_null() {
            return Err(error(E_INVALIDARG));
        }
        // Every range is placed at the guest's cursor, which follows the composition, so the
        // candidate list opens beside what is being composed. Until the guest says where it
        // is, there is no layout; the framework asks again when it is told one.
        let cursor = self.shared.cursor().ok_or_else(|| error(TS_E_NOLAYOUT))?;
        let client = self
            .shared
            .client_on_screen()
            .ok_or_else(|| error(TS_E_NOLAYOUT))?;
        // SAFETY: the framework's out pointers.
        unsafe {
            *prc = cursor_on_screen(client, cursor);
            if !pfclipped.is_null() {
                *pfclipped = false.into();
            }
        }
        Ok(())
    }

    fn GetScreenExt(&self, _vcview: u32) -> windows::core::Result<RECT> {
        self.shared
            .client_on_screen()
            .ok_or_else(|| error(TS_E_NOLAYOUT))
    }

    fn GetWnd(&self, _vcview: u32) -> windows::core::Result<HWND> {
        Ok(HWND(self.shared.hwnd.get() as *mut core::ffi::c_void))
    }
}

impl Store_Impl {
    /// Notes whether the framework asked for the input scope among `count` attributes.
    fn ask_attributes(&self, count: u32, attributes: *const GUID) {
        if attributes.is_null() {
            return;
        }
        // SAFETY: the framework's array of `count` attribute ids.
        let asked = unsafe { std::slice::from_raw_parts(attributes, count as usize) };
        if asked.contains(&GUID_PROP_INPUTSCOPE) {
            self.shared.scope_asked.set(true);
        }
    }
}

impl ITfContextOwnerCompositionSink_Impl for Store_Impl {
    fn OnStartComposition(
        &self,
        pcomposition: Ref<'_, ITfCompositionView>,
    ) -> windows::core::Result<BOOL> {
        let view = pcomposition.ok()?.clone();
        let extent = composition_extent(&view);
        if let Ok(mut composition) = self.shared.composition.try_borrow_mut() {
            *composition = Some(view);
        }
        if let Ok(mut store) = self.shared.store.try_borrow_mut() {
            let (_, end, _) = store.selection();
            store.set_composition(Some(extent.unwrap_or((end, end))));
        }
        Ok(true.into())
    }

    fn OnUpdateComposition(
        &self,
        pcomposition: Ref<'_, ITfCompositionView>,
        prangenew: Ref<'_, ITfRange>,
    ) -> windows::core::Result<()> {
        let extent = match prangenew.as_ref() {
            Some(range) => range_extent(range),
            None => pcomposition.as_ref().and_then(composition_extent),
        };
        if let (Some(extent), Ok(mut store)) = (extent, self.shared.store.try_borrow_mut()) {
            store.set_composition(Some(extent));
        }
        Ok(())
    }

    fn OnEndComposition(
        &self,
        _pcomposition: Ref<'_, ITfCompositionView>,
    ) -> windows::core::Result<()> {
        if let Ok(mut composition) = self.shared.composition.try_borrow_mut() {
            *composition = None;
        }
        if let Ok(mut store) = self.shared.store.try_borrow_mut() {
            store.set_composition(None);
        }
        Ok(())
    }
}

/// The input scopes of the focused editor, which the framework reads as an attribute of the
/// document.
#[implement(ITfInputScope)]
struct Scopes {
    scopes: &'static [InputScope],
}

impl ITfInputScope_Impl for Scopes_Impl {
    fn GetInputScopes(
        &self,
        pprginputscopes: *mut *mut InputScope,
        pccount: *mut u32,
    ) -> windows::core::Result<()> {
        if pprginputscopes.is_null() || pccount.is_null() {
            return Err(error(E_INVALIDARG));
        }
        let size = std::mem::size_of_val(self.scopes);
        // SAFETY: memory the framework frees with CoTaskMemFree, filled before it is handed
        // over.
        unsafe {
            let array = CoTaskMemAlloc(size) as *mut InputScope;
            if array.is_null() {
                return Err(error(windows::Win32::Foundation::E_OUTOFMEMORY));
            }
            std::ptr::copy_nonoverlapping(self.scopes.as_ptr(), array, self.scopes.len());
            *pprginputscopes = array;
            *pccount = self.scopes.len() as u32;
        }
        Ok(())
    }

    fn GetPhrase(
        &self,
        _ppbstrphrases: *mut *mut windows::core::BSTR,
        _pccount: *mut u32,
    ) -> windows::core::Result<()> {
        Err(error(E_NOTIMPL))
    }

    fn GetRegularExpression(&self) -> windows::core::Result<windows::core::BSTR> {
        Err(error(E_NOTIMPL))
    }

    fn GetSRGS(&self) -> windows::core::Result<windows::core::BSTR> {
        Err(error(E_NOTIMPL))
    }

    fn GetXML(&self) -> windows::core::Result<windows::core::BSTR> {
        Err(error(E_NOTIMPL))
    }
}

/// The text the framework passes, as UTF-16 code units.
fn wide_slice(text: &PCWSTR, count: u32) -> windows::core::Result<&[u16]> {
    if count == 0 {
        return Ok(&[]);
    }
    if text.is_null() {
        return Err(error(E_INVALIDARG));
    }
    // SAFETY: the framework's buffer of `count` code units, alive for the call.
    Ok(unsafe { std::slice::from_raw_parts(text.0, count as usize) })
}

fn text_change(change: TextChange) -> TS_TEXTCHANGE {
    TS_TEXTCHANGE {
        acpStart: change.start as i32,
        acpOldEnd: change.old_end as i32,
        acpNewEnd: change.new_end as i32,
    }
}

/// One window's document manager, associated with the window, and the context on it that
/// holds the window's text store.
pub struct Document {
    hwnd: isize,
    manager: ITfThreadMgr,
    client: u32,
    document: ITfDocumentMgr,
    context: RefCell<Option<ITfContext>>,
    shared: Rc<Shared>,
}

impl Document {
    /// The framework's document for the window `hwnd`. Fails when the window thread has no
    /// framework.
    pub fn attach(hwnd: isize, shared: &Rc<Shared>) -> Result<Document, String> {
        let (manager, client) = FRAMEWORK
            .with(|framework| {
                framework
                    .borrow()
                    .as_ref()
                    .map(|framework| (framework.manager.clone(), framework.client))
            })
            .ok_or("the text services framework did not start on the window thread")?;
        shared.hwnd.set(hwnd);
        // SAFETY: the framework's own objects, on its thread.
        let document = unsafe { manager.CreateDocumentMgr() }
            .map_err(|why| format!("the text services framework gave no document ({why})"))?;
        let attached = Document {
            hwnd,
            manager,
            client,
            document,
            context: RefCell::new(None),
            shared: Rc::clone(shared),
        };
        attached.push_context()?;
        // SAFETY: as above; the document the framework had for the window, if any, goes.
        let _ = unsafe {
            attached
                .manager
                .AssociateFocus(HWND(hwnd as *mut core::ffi::c_void), &attached.document)
        };
        Ok(attached)
    }

    fn push_context(&self) -> Result<(), String> {
        let store: ITextStoreACP = Store {
            shared: Rc::clone(&self.shared),
        }
        .into();
        let mut context: Option<ITfContext> = None;
        let mut cookie = 0u32;
        // SAFETY: the framework's own objects, on its thread.
        unsafe {
            self.document
                .CreateContext(self.client, 0, &store, &mut context, &mut cookie)
                .map_err(|why| format!("the text services framework gave no context ({why})"))?;
            let context = context.ok_or("the text services framework gave no context")?;
            self.document
                .Push(&context)
                .map_err(|why| format!("the text services framework took no context ({why})"))?;
            // Keys go straight to the editor where nothing is composed: a password, which no
            // input method may read, and a view that takes no text, which reads keys.
            let kind = self.shared.kind();
            if kind.is_secret() || !kind.takes_text() {
                let disabled = context
                    .cast::<ITfCompartmentMgr>()
                    .and_then(|compartments| {
                        compartments.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_DISABLED)
                    })
                    .and_then(|compartment| {
                        compartment.SetValue(self.client, &super::accessibility::variant_i4(1))
                    });
                if let Err(why) = disabled {
                    eprintln!(
                        "text input: could not keep the input method out of a password or a \
                         view that takes no text ({why})"
                    );
                }
            }
            *self.context.borrow_mut() = Some(context);
        }
        Ok(())
    }

    /// The editor takes another kind of text: the framework gets a new context, which it
    /// reads the input scope and the keyboard's state from afresh.
    pub fn kind_changed(&self) {
        self.end_composition();
        // SAFETY: the framework's own objects, on its thread.
        let _ = unsafe { self.document.Pop(TF_POPF_ALL) };
        self.context.borrow_mut().take();
        if let Err(why) = self.push_context() {
            eprintln!("text input: {why}");
        }
        // SAFETY: as above; refocusing the document makes the framework take the new context.
        unsafe {
            if self
                .manager
                .GetFocus()
                .is_ok_and(|focused| focused == self.document)
            {
                let _ = self.manager.SetFocus(&self.document);
            }
        }
    }

    /// Ends the framework's composition, which is typed as it stands.
    pub fn end_composition(&self) {
        if !self.shared.is_composing() {
            return;
        }
        let context = self.context.borrow().clone();
        if let Some(context) = context {
            if let Ok(services) = context.cast::<ITfContextOwnerCompositionServices>() {
                // SAFETY: the framework's own context, on its thread, with no lock held.
                let _ = unsafe { services.TerminateComposition(None) };
            }
        }
    }

    /// The guest described its editor.
    pub fn described(&self, editor: ImeEditor) {
        let Some(mut told) = self.shared.describe(editor) else {
            return;
        };
        let owed = self.shared.owed.take();
        told.kind |= owed.kind;
        told.ends_composition |= owed.ends_composition;
        if told.ends_composition {
            self.end_composition();
        }
        if told.kind {
            self.kind_changed();
        }
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        // SAFETY: the framework's own objects, on its thread.
        unsafe {
            let _ = self.manager.AssociateFocus(
                HWND(self.hwnd as *mut core::ffi::c_void),
                None::<&ITfDocumentMgr>,
            );
            let _ = self.document.Pop(TF_POPF_ALL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_host_core::compositor::task_windows::TaskWindows;
    use dsa_host_core::compositor::task_windows::WindowChange;
    use dsa_host_core::tasks::{Task, TaskEvent};

    /// The candidate list of a task kept at the density of a main monitor at 100% (130 dpi),
    /// whose window is on a monitor at 125% (164 dpi) left of and above the main one.
    #[test]
    fn fr6_on_a_monitor_of_another_scale_the_candidate_list_stands_beside_the_cursor_shown() {
        let mut windows = TaskWindows::new();
        windows.apply(&TaskEvent::Updated(Task {
            task_id: 4,
            package_name: "com.example.notes".into(),
            label: "Notes".into(),
            density_dpi: 130,
            bounds: dsa_host_core::tasks::Bounds {
                left: 0,
                top: 0,
                right: 810,
                bottom: 1661,
            },
            ..Task::default()
        }));
        windows.window_dpi(4, 96);
        // The editor's cursor, 650 by 1300 task pixels in.
        let cursor = ImeCursor {
            left: 650,
            top: 1300,
            right: 652,
            bottom: 1365,
        };
        windows.apply(&TaskEvent::ImeCursor { task_id: 4, cursor });
        // The window moves to the 125% monitor and shows the task at 164 per 130: the window
        // hears the cursor where it now shows it.
        let (changes, _) = windows.window_dpi(4, 120);
        let shown = changes
            .iter()
            .find_map(|change| match change {
                WindowChange::ImeCursor { cursor, .. } => Some(*cursor),
                _ => None,
            })
            .expect("the cursor again at the new scale");
        assert_eq!((shown.left, shown.top), (820, 1640));
        // The client area's top left is at (-2900, -880) on the screen.
        let client = RECT {
            left: -2900,
            top: -880,
            right: -2900 + 1022,
            bottom: -880 + 2095,
        };
        let rect = cursor_on_screen(client, shown);
        assert_eq!((rect.left, rect.top), (-2080, 760));
        assert_eq!(rect.bottom - rect.top, 82);
        assert!(rect.right > rect.left);
        // At 1:1 it would have stood 170 pixels left of and 340 above the cursor shown.
        let unscaled = cursor_on_screen(client, cursor);
        assert_eq!((unscaled.left, unscaled.top), (-2250, 420));
    }
}
