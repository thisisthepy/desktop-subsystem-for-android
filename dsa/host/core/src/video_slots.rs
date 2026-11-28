//! Decoded video pictures in memory the host shares with the guest, instead of in messages.
//!
//! A decoded 4K picture is about 11 MiB of NV12. Sent in a [`VideoFrame`] it is copied into
//! the message, through the socket and out of it again, and a 4K stream spent longer sending
//! and waiting than decoding. When the guest sees a region of host memory as physical memory
//! of its own (on Windows an HCS shared memory region backed by a named section that the
//! window manager maps too), the host writes each picture into a slot of that region and the
//! message carries only the slot's number and place ([`VideoFrame::slot`],
//! [`VideoFrame::slot_offset`]). The guest's codec HAL copies the picture from there into the
//! app's buffer and hands the slot back ([`VideoSlotReturned`]).
//!
//! A slot has one owner at a time. The host takes it ([`StreamSlots::take`]), writes it
//! ([`StreamSlots::write`]), lends it to the guest with the message ([`StreamSlots::lend`])
//! and does not write it again until the guest gives it back ([`StreamSlots::give_back`]) or
//! the connection ends (dropping the [`StreamSlots`]), which takes every slot of that stream
//! back. A guest that never gives a slot back holds at most [`SLOTS_PER_STREAM`] of them, and
//! only for as long as its connection lasts.
//!
//! The region starts with a header the host writes when it makes the region
//! ([`write_header`]): the magic [`MAGIC`], an id that is new for each region, and the
//! region's length, each eight bytes, little endian. The guest reads the id from its own
//! mapping and names it when it opens a decoder; the host uses the region for that stream only
//! when the id is the one it reads from its own mapping, so both are known to see the same
//! memory. Slots start at [`SLOTS_START`].
//!
//! A region may only reserve its size, with the header's pages committed by whoever made it.
//! Each slot's pages are then committed ([`SharedRegion::commit`]) before the host first writes
//! the slot, and only once: the slots remember which [`SLOT_ALIGNMENT`] granules are committed,
//! so the commit grows to the furthest any slot reached and no further. The guest reads a slot
//! only after the host wrote it and named it in a message, so it never reaches a page that is
//! not committed. When a commit is refused, that picture goes in its message instead, and the
//! reason is logged once for the region.
//!
//! The memory is given back when no stream uses it: once the last stream of a region ends,
//! the slots it touched are discarded ([`SharedRegion::discard`]), so a region costs physical
//! memory only while video plays. While streams play, the pages no slot holds go back when the
//! window manager gives back its spare memory ([`give_back_unused_pictures`]), so a stream that
//! stays open holds only the slots it uses. Discarding keeps the pages committed.
//!
//! Nothing here names a host operating system API. The platform supplies the region.
//!
//! [`VideoFrame`]: dsa_protocol::control::VideoFrame
//! [`VideoFrame::slot`]: dsa_protocol::control::VideoFrame::slot
//! [`VideoFrame::slot_offset`]: dsa_protocol::control::VideoFrame::slot_offset
//! [`VideoSlotReturned`]: dsa_protocol::control::VideoSlotReturned

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

/// The name the region goes by in [`GuestSharedMemory`](dsa_protocol::control::GuestSharedMemory).
pub const REGION_NAME: &str = "video_pictures";

/// The first eight bytes of the region's header.
pub const MAGIC: [u8; 8] = *b"DSAVPIC1";

/// Where the slots start: the header has the first 64 KiB to itself.
pub const SLOTS_START: usize = 64 * 1024;

/// Each slot starts on, and is a whole number of, 64 KiB: the largest page a host or guest
/// maps such memory in.
pub const SLOT_ALIGNMENT: usize = 64 * 1024;

/// The most slots one stream holds at once (written, or lent to the guest). Four 4K pictures
/// fit a region of the default size; the guest takes a picture as soon as its app has a buffer
/// for it, so a stream seldom has more than two lent.
pub const SLOTS_PER_STREAM: usize = 4;

/// The region's size unless the user's settings give another: room for [`SLOTS_PER_STREAM`]
/// 4K pictures (4096 by 2304 NV12, 13.5 MiB each) after the header.
pub const DEFAULT_REGION_BYTES: u64 = 64 * 1024 * 1024;

/// The smallest region worth making: the header and one 1080p picture.
pub const MIN_REGION_BYTES: u64 = (SLOTS_START + 4 * 1024 * 1024) as u64;

/// Memory the host shares with the guest, mapped into this process.
pub trait SharedRegion: Send + Sync {
    /// The start of this process's view of the region.
    fn as_ptr(&self) -> *mut u8;
    /// The view's length in bytes.
    fn len(&self) -> usize;
    /// Whether the view is empty; never, for a region that can hold a slot.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Nothing uses `len` bytes from `offset` now: the platform may give their physical pages
    /// back. Their contents need not survive.
    fn discard(&self, offset: usize, len: usize) {
        let _ = (offset, len);
    }
    /// Makes `len` bytes from `offset` ready to be written. A region that only reserves its
    /// size (on Windows, a section made `SEC_RESERVE`) commits their pages here, since writing
    /// a page that is only reserved faults. An error says why they could not be (the system's
    /// commit limit, say); nothing is written there then. Memory that is always there needs
    /// nothing, which is the default.
    fn commit(&self, offset: usize, len: usize) -> Result<(), String> {
        let _ = (offset, len);
        Ok(())
    }
}

/// Writes the region's header into its first bytes: [`MAGIC`], `id` and `len`.
pub fn write_header(bytes: &mut [u8], id: u64, len: u64) -> bool {
    if bytes.len() < 24 {
        return false;
    }
    bytes[..8].copy_from_slice(&MAGIC);
    bytes[8..16].copy_from_slice(&id.to_le_bytes());
    bytes[16..24].copy_from_slice(&len.to_le_bytes());
    true
}

/// The id and the length a region's header gives, or `None` when it has no header.
pub fn read_header(bytes: &[u8]) -> Option<(u64, u64)> {
    if bytes.len() < 24 || bytes[..8] != MAGIC {
        return None;
    }
    let id = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
    let len = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
    Some((id, len))
}

/// A new id for a region: random, and never 0 (which says "no region").
pub fn new_region_id() -> u64 {
    let mut bytes = [0u8; 8];
    loop {
        if getrandom::getrandom(&mut bytes).is_err() {
            // Without the system's randomness, the time still makes a new id for each region.
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(1);
            bytes = (nanos ^ u64::from(std::process::id())).to_le_bytes();
        }
        let id = u64::from_le_bytes(bytes);
        if id != 0 {
            return id;
        }
    }
}

/// A slot of the region: its number, which the guest gives back, and the bytes it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub number: u32,
    pub offset: u64,
    /// The bytes of the picture it holds; the slot itself is this rounded up to
    /// [`SLOT_ALIGNMENT`].
    pub len: usize,
}

struct Held {
    /// The bytes the slot covers, a whole number of [`SLOT_ALIGNMENT`].
    span: usize,
    stream: u64,
    number: u32,
    /// Sent to the guest: the host does not write it until it comes back.
    lent: bool,
}

#[derive(Default)]
struct State {
    /// The slots held now, by offset.
    held: BTreeMap<usize, Held>,
    next_number: u32,
    next_stream: u64,
    streams: usize,
    /// The furthest byte any slot reached since the region was last discarded.
    touched_end: usize,
    /// Which [`SLOT_ALIGNMENT`] granules of the region are committed, by index from the
    /// region's start. Discarding a slot keeps its granules committed.
    committed: Vec<bool>,
    /// Which granules a slot was written in since their pages were last given back, by index
    /// from the region's start: the ones that may hold physical pages.
    written: Vec<bool>,
    /// Why a commit was first refused, once one was: the reason is logged that once.
    commit_refused: Option<String>,
}

impl State {
    /// Commits the granules of `span` bytes from `offset` that are not committed yet, a run of
    /// them at a time, and marks them. Stops at the first run the region refuses.
    fn commit(&mut self, region: &dyn SharedRegion, offset: usize, span: usize) -> bool {
        let first = offset / SLOT_ALIGNMENT;
        let end = (offset + span).div_ceil(SLOT_ALIGNMENT);
        if self.committed.len() < end {
            self.committed.resize(end, false);
        }
        let mut at = first;
        while at < end {
            if self.committed[at] {
                at += 1;
                continue;
            }
            let run_end = (at..end).find(|&i| self.committed[i]).unwrap_or(end);
            let bytes = (run_end - at) * SLOT_ALIGNMENT;
            match region.commit(at * SLOT_ALIGNMENT, bytes) {
                Ok(()) => self.committed[at..run_end].fill(true),
                Err(why) => {
                    if self.commit_refused.is_none() {
                        eprintln!(
                            "video: a picture goes to the guest in its message: the shared \
                             picture memory for its slot could not be committed ({why})"
                        );
                        self.commit_refused = Some(why);
                    }
                    return false;
                }
            }
            at = run_end;
        }
        true
    }
}

/// The regions open in this process, for [`give_back_unused_pictures`].
static OPEN: Mutex<Vec<Weak<PictureSlots>>> = Mutex::new(Vec::new());

/// What the shared picture memory of this process came to: the bytes some slot reached
/// before, and the bytes given back now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PicturesGivenBack {
    pub touched: usize,
    pub given_back: usize,
}

/// Gives back the pages of every open region that no slot holds now
/// ([`PictureSlots::give_back_unused`]), for the window manager's periodic give-back while
/// windows are shown.
pub fn give_back_unused_pictures() -> PicturesGivenBack {
    let open: Vec<Arc<PictureSlots>> = OPEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter_map(Weak::upgrade)
        .collect();
    let mut total = PicturesGivenBack::default();
    for slots in open {
        total.touched += slots.touched();
        total.given_back += slots.give_back_unused();
    }
    total
}

/// The slots of one region, shared by every stream that sends its pictures there.
pub struct PictureSlots {
    region: Arc<dyn SharedRegion>,
    id: u64,
    state: Mutex<State>,
}

impl PictureSlots {
    /// The slots of `region`, whose header the host wrote. An error says why the region cannot
    /// carry pictures.
    pub fn open(region: Arc<dyn SharedRegion>) -> Result<Arc<PictureSlots>, String> {
        let len = region.len();
        if (len as u64) < MIN_REGION_BYTES {
            return Err(format!(
                "the shared picture region of {len} bytes is smaller than {MIN_REGION_BYTES}"
            ));
        }
        // The maker of the region committed the header's pages already, so this costs nothing;
        // it makes the read below certain to reach committed memory.
        region.commit(0, SLOTS_START).map_err(|why| {
            format!("the shared picture region's header could not be committed ({why})")
        })?;
        // SAFETY: the view is `len` bytes long and outlives this read.
        let header = unsafe { std::slice::from_raw_parts(region.as_ptr(), 24) };
        let (id, said) = read_header(header).ok_or_else(|| {
            "the shared picture region has no header; the service did not make it".to_string()
        })?;
        if said > len as u64 || id == 0 {
            return Err(format!(
                "the shared picture region's header says {said} bytes and id {id:x}, but the \
                 view has {len}"
            ));
        }
        let slots = Arc::new(PictureSlots {
            region,
            id,
            state: Mutex::new(State {
                next_number: 1,
                committed: vec![true; SLOTS_START / SLOT_ALIGNMENT],
                ..State::default()
            }),
        });
        let mut open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
        open.retain(|slots| slots.strong_count() > 0);
        open.push(Arc::downgrade(&slots));
        Ok(slots)
    }

    /// Gives back the pages no slot holds now, while streams still play: the slots of a
    /// picture size a stream no longer sends (a 4K picture between 1080p ones, a second
    /// stream's) and the ones placed further up while others were lent. Before, the pages any
    /// slot ever reached stayed in this process's working set until the last stream ended,
    /// which a window that keeps a player open never does. Returns the bytes given back.
    ///
    /// The state stays locked while the pages go, so no slot is taken in them meanwhile; a slot
    /// taken later writes its pages again, as after the last stream ends.
    pub fn give_back_unused(&self) -> usize {
        let mut state = self.state();
        let end = state.touched_end;
        if end <= SLOTS_START {
            return 0;
        }
        let granules = end.div_ceil(SLOT_ALIGNMENT);
        let mut unused: Vec<bool> = (0..granules)
            .map(|i| {
                i >= SLOTS_START / SLOT_ALIGNMENT
                    && state.committed.get(i).copied().unwrap_or(false)
                    && state.written.get(i).copied().unwrap_or(false)
            })
            .collect();
        let mut held_end = 0;
        for (&offset, held) in &state.held {
            held_end = held_end.max(offset + held.span);
            let last = (offset + held.span).div_ceil(SLOT_ALIGNMENT).min(granules);
            for granule in &mut unused[(offset / SLOT_ALIGNMENT).min(last)..last] {
                *granule = false;
            }
        }
        let runs = committed_runs(&unused, SLOTS_START, end);
        for (granule, gone) in state.written.iter_mut().zip(&unused) {
            if *gone {
                *granule = false;
            }
        }
        state.touched_end = held_end;
        for &(offset, len) in &runs {
            self.region.discard(offset, len);
        }
        runs.iter().map(|&(_, len)| len).sum()
    }

    /// The bytes of the region some slot reached since its pages were last given back: what
    /// this process may hold of it now.
    pub fn touched(&self) -> usize {
        self.state().touched_end.saturating_sub(SLOTS_START)
    }

    /// The id in the region's header, which the guest names when it maps the same region.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The bytes the slots may use, after the header.
    pub fn capacity(&self) -> usize {
        self.region.len().saturating_sub(SLOTS_START) / SLOT_ALIGNMENT * SLOT_ALIGNMENT
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A stream that sends its pictures here. Its slots come back when it is dropped.
    pub fn stream(self: &Arc<Self>) -> StreamSlots {
        let mut state = self.state();
        let stream = state.next_stream;
        state.next_stream += 1;
        state.streams += 1;
        StreamSlots {
            slots: Arc::clone(self),
            stream,
        }
    }

    /// How many slots are held now, by every stream.
    pub fn held(&self) -> usize {
        self.state().held.len()
    }

    /// Why the region first refused to commit a slot's pages, if it ever did.
    pub fn commit_refused(&self) -> Option<String> {
        self.state().commit_refused.clone()
    }
}

/// One stream's use of the region: the slots it takes, writes, lends to the guest and gets
/// back. Dropping it takes back every slot it still holds.
pub struct StreamSlots {
    slots: Arc<PictureSlots>,
    stream: u64,
}

impl StreamSlots {
    /// The id of the region.
    pub fn region_id(&self) -> u64 {
        self.slots.id
    }

    /// How many slots this stream holds.
    pub fn held(&self) -> usize {
        let state = self.slots.state();
        state
            .held
            .values()
            .filter(|h| h.stream == self.stream)
            .count()
    }

    /// A slot for a picture of `len` bytes, or `None` when this stream already holds
    /// [`SLOTS_PER_STREAM`] or the region has no room: the picture then goes in its message.
    pub fn take(&self, len: usize) -> Option<Slot> {
        if len == 0 {
            return None;
        }
        let span = len.checked_next_multiple_of(SLOT_ALIGNMENT)?;
        let end = SLOTS_START + self.slots.capacity();
        let mut state = self.slots.state();
        let mine = state
            .held
            .values()
            .filter(|h| h.stream == self.stream)
            .count();
        if mine >= SLOTS_PER_STREAM {
            return None;
        }
        // The first gap that fits, in order of offset.
        let mut at = SLOTS_START;
        for (&offset, held) in &state.held {
            if offset >= at + span {
                break;
            }
            at = at.max(offset + held.span);
        }
        if at + span > end {
            return None;
        }
        let number = state.next_number;
        state.next_number = state.next_number.wrapping_add(1).max(1);
        state.held.insert(
            at,
            Held {
                span,
                stream: self.stream,
                number,
                lent: false,
            },
        );
        state.touched_end = state.touched_end.max(at + span);
        Some(Slot {
            number,
            offset: at as u64,
            len,
        })
    }

    /// Lets `fill` write the slot's bytes, while this stream holds it and has not lent it.
    /// Returns what `fill` returned, or false when the slot is not this stream's to write or
    /// its pages could not be committed (the picture then goes in its message). The slot's
    /// pages are committed first, the first time any slot covers them.
    pub fn write(&self, slot: &Slot, fill: impl FnOnce(&mut [u8]) -> bool) -> bool {
        let offset = slot.offset as usize;
        {
            let mut state = self.slots.state();
            let span = match state.held.get(&offset) {
                Some(held)
                    if held.stream == self.stream
                        && held.number == slot.number
                        && !held.lent
                        && slot.len <= held.span =>
                {
                    held.span
                }
                _ => return false,
            };
            if !state.commit(self.slots.region.as_ref(), offset, span) {
                return false;
            }
            let (first, end) = (
                offset / SLOT_ALIGNMENT,
                (offset + span).div_ceil(SLOT_ALIGNMENT),
            );
            if state.written.len() < end {
                state.written.resize(end, false);
            }
            state.written[first..end].fill(true);
        }
        // SAFETY: the slot's bytes lie inside the view (`take` placed them there), and no one
        // else writes them: the slot is this stream's, and not lent, until this stream lends it
        // or gives it back, which takes `&self` on this same stream after `fill` returns.
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(self.slots.region.as_ptr().add(offset), slot.len)
        };
        fill(bytes)
    }

    /// Lets `look` read the slot's bytes, while this stream holds it, has not lent it and its
    /// pages are committed (a slot is committed before it is written). `None` otherwise.
    pub fn read<R>(&self, slot: &Slot, look: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let offset = slot.offset as usize;
        {
            let state = self.slots.state();
            match state.held.get(&offset) {
                Some(held)
                    if held.stream == self.stream
                        && held.number == slot.number
                        && !held.lent
                        && slot.len <= held.span => {}
                _ => return None,
            }
            let (first, end) = (
                offset / SLOT_ALIGNMENT,
                (offset + slot.len).div_ceil(SLOT_ALIGNMENT),
            );
            if state
                .committed
                .get(first..end)
                .is_none_or(|c| !c.iter().all(|&c| c))
            {
                return None;
            }
        }
        // SAFETY: the slot's bytes lie inside the view and are committed, and no one writes
        // them meanwhile: the slot is this stream's and not lent, and only this stream writes
        // it, through `&self` on the same thread.
        let bytes =
            unsafe { std::slice::from_raw_parts(self.slots.region.as_ptr().add(offset), slot.len) };
        Some(look(bytes))
    }

    /// The slot goes to the guest with its picture's message: the host does not write it until
    /// the guest gives it back. False when it is not this stream's.
    pub fn lend(&self, slot: &Slot) -> bool {
        let mut state = self.slots.state();
        match state.held.get_mut(&(slot.offset as usize)) {
            Some(held) if held.stream == self.stream && held.number == slot.number => {
                held.lent = true;
                true
            }
            _ => false,
        }
    }

    /// The slot numbered `number` is this stream's to reuse: the guest gave it back, or the
    /// host dropped its picture before sending it. False for a number this stream does not
    /// hold, which changes nothing.
    pub fn give_back(&self, number: u32) -> bool {
        let mut state = self.slots.state();
        let found = state
            .held
            .iter()
            .find(|(_, h)| h.stream == self.stream && h.number == number)
            .map(|(&offset, _)| offset);
        match found {
            Some(offset) => {
                state.held.remove(&offset);
                true
            }
            None => false,
        }
    }
}

impl Drop for StreamSlots {
    fn drop(&mut self) {
        let mut state = self.slots.state();
        state.held.retain(|_, h| h.stream != self.stream);
        state.streams = state.streams.saturating_sub(1);
        if state.streams == 0 && state.touched_end > SLOTS_START {
            let end = std::mem::take(&mut state.touched_end);
            state.written.clear();
            // The last stream is gone: what the pictures touched goes back to the host. Only
            // committed pages were written, and only they are given back; their commit stays.
            // The lock is held throughout: a stream that opens meanwhile (an app that releases
            // its decoder and makes another at once, as a player moving to a new surface does)
            // takes and writes its first slot only after the discard, whose contents are
            // undefined, so its first picture never reaches the guest as zeros.
            for (offset, len) in committed_runs(&state.committed, SLOTS_START, end) {
                self.slots.region.discard(offset, len);
            }
        }
    }
}

/// The committed runs of granules between the bytes `start` and `end`, as `(offset, len)`.
fn committed_runs(committed: &[bool], start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut run: Option<usize> = None;
    let last = end.div_ceil(SLOT_ALIGNMENT);
    for index in start / SLOT_ALIGNMENT..=last {
        let on = index < last && committed.get(index).copied().unwrap_or(false);
        match (on, run) {
            (true, None) => run = Some(index),
            (false, Some(first)) => {
                runs.push((first * SLOT_ALIGNMENT, (index - first) * SLOT_ALIGNMENT));
                run = None;
            }
            _ => {}
        }
    }
    runs
}

/// A region in this process's own memory, for tests and for hosts that share none: it is
/// what the platform's region would be, without a guest to see it.
pub struct MemoryRegion {
    bytes: Box<[std::cell::UnsafeCell<u8>]>,
    discarded: Mutex<Vec<(usize, usize)>>,
}

// SAFETY: the bytes are only written through `StreamSlots::write`, whose slots never overlap.
unsafe impl Sync for MemoryRegion {}
// SAFETY: as above; the bytes are owned.
unsafe impl Send for MemoryRegion {}

impl MemoryRegion {
    /// A region of `len` bytes with its header written, id `id`.
    pub fn new(len: usize, id: u64) -> MemoryRegion {
        let bytes: Vec<std::cell::UnsafeCell<u8>> =
            (0..len).map(|_| std::cell::UnsafeCell::new(0)).collect();
        let region = MemoryRegion {
            bytes: bytes.into_boxed_slice(),
            discarded: Mutex::new(Vec::new()),
        };
        // SAFETY: the region is new and this is the only reference.
        let all = unsafe { std::slice::from_raw_parts_mut(region.as_ptr(), len) };
        write_header(all, id, len as u64);
        region
    }

    /// The bytes from `offset`, as the guest would read them.
    pub fn read(&self, offset: usize, len: usize) -> Vec<u8> {
        let end = (offset + len).min(self.bytes.len());
        // SAFETY: inside the region; a copy taken while nothing writes it in the tests.
        unsafe { std::slice::from_raw_parts(self.as_ptr().add(offset), end - offset) }.to_vec()
    }

    /// Every range discarded so far.
    pub fn discarded(&self) -> Vec<(usize, usize)> {
        self.discarded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl SharedRegion for MemoryRegion {
    fn as_ptr(&self) -> *mut u8 {
        std::cell::UnsafeCell::raw_get(self.bytes.as_ptr())
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn discard(&self, offset: usize, len: usize) {
        self.discarded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((offset, len));
    }
}
