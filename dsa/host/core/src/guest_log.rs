//! The guest's log as the host reads it: a stream that flows only while someone reads it.
//!
//! Android keeps its log in logd's buffers in memory. The guest's channel daemon reads them
//! only while the host asks it to ([`control::LogStream`]) and sends the records on a vsock
//! port of their own, so a busy log never delays input or window messages on the control
//! channel. Nothing here writes a log file.
//!
//! - [`LogEntry`]: one record, checked on the way in ([`LogEntry::from_control`]); a record
//!   that breaks the rules ends the log connection it came on, never the service.
//! - [`LogFilter`]: which records a reader wants, as logcat's buffers and `TAG:P` specs.
//! - [`LogRing`]: the small in-memory history ([`RING_BYTES`]) a new reader starts with.
//! - [`LogHub`]: the readers, what the guest must send to serve all of them, and the
//!   one-off dumps and diagnostics under way.
//! - [`serve_log_connection`]: one authenticated connection on the log port.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::time::Duration;

use dsa_protocol::control::{self, envelope::Body};
use dsa_protocol::host_api::types as wire;
use dsa_transport::{Connection, TransportError};

/// The most bytes of records the host keeps for readers that join later. Records past it are
/// dropped oldest first; a reader that wants more asks Android for a dump.
pub const RING_BYTES: usize = 1024 * 1024;

/// The most records one reader may have waiting. A reader that falls this far behind loses
/// records, and is told how many with a record that stands for them.
pub const READER_QUEUE: usize = 8192;

/// The most records one frame from the guest may carry.
pub const MAX_RECORDS_PER_FRAME: usize = 4096;

/// The longest tag a record may have, in bytes. logd keeps less.
pub const MAX_TAG_BYTES: usize = 1024;

/// The longest message a record may have, in bytes. logd keeps less than this.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// How many of logd's newest records the guest sends first when the stream starts, so a
/// reader that joins sees what led up to now.
pub const STREAM_TAIL: u32 = 1000;

/// Android's priorities.
pub const VERBOSE: u8 = 2;
pub const DEBUG: u8 = 3;
pub const INFO: u8 = 4;
pub const WARN: u8 = 5;
pub const ERROR: u8 = 6;
pub const FATAL: u8 = 7;
/// Above every priority: a tag at this level shows nothing.
pub const SILENT: u8 = 8;

/// One of logd's buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LogBuffer {
    Main,
    Radio,
    Events,
    System,
    Crash,
    Stats,
    Security,
    Kernel,
}

impl LogBuffer {
    pub const ALL: [LogBuffer; 8] = [
        LogBuffer::Main,
        LogBuffer::Radio,
        LogBuffer::Events,
        LogBuffer::System,
        LogBuffer::Crash,
        LogBuffer::Stats,
        LogBuffer::Security,
        LogBuffer::Kernel,
    ];

    /// The buffer's number on both wires: logd's log ID plus one.
    pub fn number(self) -> u32 {
        match self {
            LogBuffer::Main => 1,
            LogBuffer::Radio => 2,
            LogBuffer::Events => 3,
            LogBuffer::System => 4,
            LogBuffer::Crash => 5,
            LogBuffer::Stats => 6,
            LogBuffer::Security => 7,
            LogBuffer::Kernel => 8,
        }
    }

    pub fn from_number(number: u32) -> Option<LogBuffer> {
        LogBuffer::ALL.into_iter().find(|b| b.number() == number)
    }

    /// The buffer's bit in a set of buffers.
    pub fn bit(self) -> u32 {
        1 << self.number()
    }

    /// logcat's name for the buffer.
    pub fn name(self) -> &'static str {
        match self {
            LogBuffer::Main => "main",
            LogBuffer::Radio => "radio",
            LogBuffer::Events => "events",
            LogBuffer::System => "system",
            LogBuffer::Crash => "crash",
            LogBuffer::Stats => "stats",
            LogBuffer::Security => "security",
            LogBuffer::Kernel => "kernel",
        }
    }

    pub fn from_name(name: &str) -> Option<LogBuffer> {
        LogBuffer::ALL.into_iter().find(|b| b.name() == name)
    }
}

/// The buffers logcat reads when none is named: main, system and crash.
pub const DEFAULT_BUFFERS: u32 = (1 << 1) | (1 << 4) | (1 << 5);

/// Every buffer.
pub const ALL_BUFFERS: u32 = 0b1_1111_1110;

/// logcat's letter for a priority.
pub fn priority_letter(priority: u8) -> char {
    match priority {
        0 | 1 | VERBOSE => 'V',
        DEBUG => 'D',
        INFO => 'I',
        WARN => 'W',
        ERROR => 'E',
        FATAL => 'F',
        _ => 'S',
    }
}

/// The priority a logcat letter names.
pub fn priority_from_letter(letter: &str) -> Option<u8> {
    Some(match letter.to_ascii_uppercase().as_str() {
        "V" | "*" => VERBOSE,
        "D" => DEBUG,
        "I" => INFO,
        "W" => WARN,
        "E" => ERROR,
        "F" => FATAL,
        "S" => SILENT,
        _ => return None,
    })
}

/// One record of the guest's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub buffer: LogBuffer,
    pub priority: u8,
    /// Nanoseconds since the Unix epoch, by the guest's clock.
    pub time_ns: u64,
    pub pid: i32,
    pub tid: i32,
    pub uid: u32,
    pub tag: String,
    pub message: String,
    /// Not 0: this record stands for that many records that were dropped.
    pub dropped: u64,
}

impl LogEntry {
    /// The record that stands for `count` records dropped.
    pub fn dropped(count: u64) -> LogEntry {
        LogEntry {
            buffer: LogBuffer::Main,
            priority: WARN,
            time_ns: 0,
            pid: 0,
            tid: 0,
            uid: 0,
            tag: String::new(),
            message: String::new(),
            dropped: count,
        }
    }

    /// A record as the guest sent it, checked. An error says what is wrong with it; a guest
    /// that sends one is broken or hostile, and its log connection is closed.
    pub fn from_control(record: control::LogRecord) -> Result<LogEntry, String> {
        if record.dropped > 0 {
            return Ok(LogEntry::dropped(record.dropped));
        }
        let buffer = u32::try_from(record.buffer)
            .ok()
            .and_then(LogBuffer::from_number)
            .ok_or_else(|| format!("a record names buffer {}, which logd has not", record.buffer))?;
        Self::checked(
            buffer,
            record.priority,
            record.time_ns,
            record.pid,
            record.tid,
            record.uid,
            record.tag,
            record.message,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn checked(
        buffer: LogBuffer,
        priority: u32,
        time_ns: u64,
        pid: i32,
        tid: i32,
        uid: u32,
        tag: String,
        message: String,
    ) -> Result<LogEntry, String> {
        let priority = u8::try_from(priority)
            .ok()
            .filter(|p| *p <= SILENT)
            .ok_or_else(|| format!("a record has priority {priority}, which Android has not"))?;
        if tag.len() > MAX_TAG_BYTES {
            return Err(format!(
                "a record's tag is {} bytes, more than the {MAX_TAG_BYTES} allowed",
                tag.len()
            ));
        }
        if message.len() > MAX_MESSAGE_BYTES {
            return Err(format!(
                "a record's message is {} bytes, more than the {MAX_MESSAGE_BYTES} allowed",
                message.len()
            ));
        }
        Ok(LogEntry {
            buffer,
            priority,
            time_ns,
            pid,
            tid,
            uid,
            tag,
            message,
            dropped: 0,
        })
    }

    pub fn to_control(&self) -> control::LogRecord {
        control::LogRecord {
            buffer: if self.dropped > 0 {
                0
            } else {
                self.buffer.number() as i32
            },
            priority: if self.dropped > 0 {
                0
            } else {
                u32::from(self.priority)
            },
            time_ns: self.time_ns,
            pid: self.pid,
            tid: self.tid,
            uid: self.uid,
            tag: self.tag.clone(),
            message: self.message.clone(),
            dropped: self.dropped,
        }
    }

    pub fn to_host_api(&self) -> wire::LogEntry {
        wire::LogEntry {
            buffer: if self.dropped > 0 {
                0
            } else {
                self.buffer.number()
            },
            priority: if self.dropped > 0 {
                0
            } else {
                u32::from(self.priority)
            },
            time_ns: self.time_ns,
            pid: self.pid,
            tid: self.tid,
            uid: self.uid,
            tag: self.tag.clone(),
            message: self.message.clone(),
            dropped: self.dropped,
        }
    }

    /// A record as the service sent it, checked the same way.
    pub fn from_host_api(entry: wire::LogEntry) -> Result<LogEntry, String> {
        if entry.dropped > 0 {
            return Ok(LogEntry::dropped(entry.dropped));
        }
        let buffer = LogBuffer::from_number(entry.buffer)
            .ok_or_else(|| format!("a record names buffer {}, which logd has not", entry.buffer))?;
        Self::checked(
            buffer,
            entry.priority,
            entry.time_ns,
            entry.pid,
            entry.tid,
            entry.uid,
            entry.tag,
            entry.message,
        )
    }

    /// About how much memory the record takes, which is what the ring counts.
    pub fn size(&self) -> usize {
        48 + self.tag.len() + self.message.len()
    }

    /// The record as logcat's `threadtime` format prints it, with the year and in UTC, one
    /// line per line of its message.
    pub fn threadtime(&self) -> String {
        if self.dropped > 0 {
            return format!("--------- {} records dropped", self.dropped);
        }
        let stamp = utc_time(self.time_ns);
        let letter = priority_letter(self.priority);
        let mut out = String::new();
        let message = self.message.trim_end_matches('\n');
        for (i, line) in message.split('\n').enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&format!(
                "{stamp} {:5} {:5} {letter} {}: {line}",
                self.pid, self.tid, self.tag
            ));
        }
        out
    }
}

/// `time_ns` as `YYYY-MM-DD hh:mm:ss.mmm` in UTC.
pub fn utc_time(time_ns: u64) -> String {
    let seconds = time_ns / 1_000_000_000;
    let millis = (time_ns / 1_000_000) % 1000;
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Days since 1970-01-01 to a civil date, after Howard Hinnant's `civil_from_days`.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{millis:03}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Which records a reader wants: the buffers, and the least priority of each tag, as
/// logcat's `-b` and `TAG:P` arguments say them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFilter {
    /// Bit ([`LogBuffer::bit`]) for each buffer.
    pub buffers: u32,
    /// Each tag's least priority. The tag `*` sets it for every tag not named; without it
    /// they show at every priority.
    pub tags: Vec<(String, u8)>,
}

impl Default for LogFilter {
    fn default() -> Self {
        LogFilter {
            buffers: DEFAULT_BUFFERS,
            tags: Vec::new(),
        }
    }
}

impl LogFilter {
    /// Reads one `TAG:P` spec (`P` one of V, D, I, W, E, F, S); a tag alone shows at every
    /// priority.
    pub fn parse_spec(spec: &str) -> Result<(String, u8), String> {
        let (tag, letter) = match spec.rsplit_once(':') {
            Some((tag, letter)) => (tag, letter),
            None => (spec, "V"),
        };
        if tag.is_empty() {
            return Err(format!("{spec:?} names no tag; write TAG:P, such as ActivityManager:I"));
        }
        let priority = priority_from_letter(letter).ok_or_else(|| {
            format!("{spec:?} has no priority Android knows; P is one of V, D, I, W, E, F or S")
        })?;
        Ok((tag.to_string(), priority))
    }

    /// The buffers named, comma separated, as logcat's `-b` takes them; `all` is every one.
    pub fn parse_buffers(names: &str) -> Result<u32, String> {
        let mut buffers = 0;
        for name in names.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            if name == "all" {
                buffers |= ALL_BUFFERS;
                continue;
            }
            if name == "default" {
                buffers |= DEFAULT_BUFFERS;
                continue;
            }
            let buffer = LogBuffer::from_name(name).ok_or_else(|| {
                format!(
                    "there is no log buffer {name:?}; the buffers are main, radio, events, \
                     system, crash, stats, security, kernel and all"
                )
            })?;
            buffers |= buffer.bit();
        }
        if buffers == 0 {
            return Err("name at least one log buffer".into());
        }
        Ok(buffers)
    }

    /// The least priority of a tag no spec names.
    pub fn default_priority(&self) -> u8 {
        self.tags
            .iter()
            .rev()
            .find(|(tag, _)| tag == "*")
            .map(|(_, p)| *p)
            .unwrap_or(VERBOSE)
    }

    fn tag_priority(&self, tag: &str) -> u8 {
        self.tags
            .iter()
            .rev()
            .find(|(name, _)| name == tag)
            .map(|(_, p)| *p)
            .unwrap_or_else(|| self.default_priority())
    }

    /// Whether `entry` passes. A record that stands for dropped records always does, so the
    /// reader learns of the gap.
    pub fn passes(&self, entry: &LogEntry) -> bool {
        if entry.dropped > 0 {
            return true;
        }
        self.buffers & entry.buffer.bit() != 0 && entry.priority >= self.tag_priority(&entry.tag)
    }

    /// The least priority any record that passes can have.
    pub fn least_priority(&self) -> u8 {
        self.tags
            .iter()
            .map(|(_, p)| *p)
            .chain(std::iter::once(self.default_priority()))
            .min()
            .unwrap_or(VERBOSE)
    }

    pub fn to_host_api(&self, dump: bool) -> wire::WatchLog {
        wire::WatchLog {
            buffers: self.buffers,
            tags: self
                .tags
                .iter()
                .map(|(tag, p)| wire::LogTagFilter {
                    tag: tag.clone(),
                    min_priority: u32::from(*p),
                })
                .collect(),
            dump,
        }
    }

    /// A filter as a client sent it, checked: no buffer means logcat's default ones.
    pub fn from_host_api(watch: &wire::WatchLog) -> Result<LogFilter, String> {
        if watch.buffers & !ALL_BUFFERS != 0 {
            return Err(format!("the buffers {:#x} include one logd has not", watch.buffers));
        }
        if watch.tags.len() > 256 {
            return Err(format!("{} tag filters is more than the 256 allowed", watch.tags.len()));
        }
        let mut tags = Vec::with_capacity(watch.tags.len());
        for filter in &watch.tags {
            if filter.tag.is_empty() || filter.tag.len() > MAX_TAG_BYTES {
                return Err("a tag filter names no tag, or one too long".into());
            }
            let priority = u8::try_from(filter.min_priority)
                .ok()
                .filter(|p| *p <= SILENT)
                .ok_or_else(|| format!("a tag filter has priority {}", filter.min_priority))?;
            tags.push((filter.tag.clone(), priority));
        }
        Ok(LogFilter {
            buffers: if watch.buffers == 0 {
                DEFAULT_BUFFERS
            } else {
                watch.buffers
            },
            tags,
        })
    }
}

/// The newest records, up to a number of bytes.
#[derive(Debug, Clone)]
pub struct LogRing {
    entries: VecDeque<LogEntry>,
    bytes: usize,
    limit: usize,
}

impl LogRing {
    pub fn new(limit: usize) -> LogRing {
        LogRing {
            entries: VecDeque::new(),
            bytes: 0,
            limit,
        }
    }

    /// Adds `entry`, dropping the oldest records until the ring is within its limit. A record
    /// larger than the whole ring is not kept.
    pub fn push(&mut self, entry: LogEntry) {
        let size = entry.size();
        if size > self.limit {
            return;
        }
        while self.bytes + size > self.limit {
            match self.entries.pop_front() {
                Some(old) => self.bytes -= old.size(),
                None => break,
            }
        }
        self.bytes += size;
        self.entries.push_back(entry);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The bytes the records take, as [`LogEntry::size`] counts them.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn iter(&self) -> impl Iterator<Item = &LogEntry> {
        self.entries.iter()
    }
}

/// What a dump of Android's log delivers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogDumpEvent {
    Entries(Vec<LogEntry>),
    /// The dump is over; the text says why it ended early, if it did.
    End(Option<String>),
}

/// What the guest's diagnostics deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticsEvent {
    /// A piece of a file; `last` ends the file.
    File {
        name: String,
        data: Vec<u8>,
        last: bool,
    },
    /// The guest has sent everything; the text names what it could not gather.
    Done(Option<String>),
}

struct Reader {
    id: u64,
    filter: LogFilter,
    queue: SyncSender<LogEntry>,
    /// Records this reader lost since the last one it was given.
    dropped: u64,
}

/// The readers of one guest's log, its recent history, and the dumps and diagnostics under
/// way. A session keeps one for the life of the service, across restarts of the guest.
pub struct LogHub {
    ring: LogRing,
    readers: Vec<Reader>,
    next_reader: u64,
    dumps: HashMap<u32, (LogFilter, Sender<LogDumpEvent>)>,
    diagnostics: HashMap<u32, Sender<DiagnosticsEvent>>,
    next_request: u32,
    queue: usize,
}

impl Default for LogHub {
    fn default() -> Self {
        LogHub::new(RING_BYTES, READER_QUEUE)
    }
}

impl LogHub {
    /// A hub whose ring keeps `ring_bytes` and whose readers each have `queue` records of room.
    pub fn new(ring_bytes: usize, queue: usize) -> LogHub {
        LogHub {
            ring: LogRing::new(ring_bytes),
            readers: Vec::new(),
            next_reader: 0,
            dumps: HashMap::new(),
            diagnostics: HashMap::new(),
            next_request: 0,
            queue: queue.max(1),
        }
    }

    /// Adds a reader. Returns its ID, the records of the ring that pass its filter, and where
    /// new ones arrive.
    pub fn subscribe(&mut self, filter: LogFilter) -> (u64, Vec<LogEntry>, Receiver<LogEntry>) {
        let history = self
            .ring
            .iter()
            .filter(|e| filter.passes(e))
            .cloned()
            .collect();
        let (queue, entries) = mpsc::sync_channel(self.queue);
        self.next_reader += 1;
        let id = self.next_reader;
        self.readers.push(Reader {
            id,
            filter,
            queue,
            dropped: 0,
        });
        (id, history, entries)
    }

    /// Removes a reader. Returns whether it was there.
    pub fn unsubscribe(&mut self, id: u64) -> bool {
        let before = self.readers.len();
        self.readers.retain(|r| r.id != id);
        self.readers.len() != before
    }

    pub fn reader_count(&self) -> usize {
        self.readers.len()
    }

    /// What the guest must send to serve every reader: off when there is none, else every
    /// buffer any reader wants down to the least priority any of them wants.
    pub fn stream(&self) -> control::LogStream {
        if self.readers.is_empty() {
            return control::LogStream::default();
        }
        let buffers = self.readers.iter().fold(0, |all, r| all | r.filter.buffers);
        let min_priority = self
            .readers
            .iter()
            .map(|r| r.filter.least_priority())
            .min()
            .unwrap_or(VERBOSE);
        control::LogStream {
            on: true,
            buffers,
            min_priority: u32::from(min_priority.min(FATAL)),
            tail: if self.ring.is_empty() { STREAM_TAIL } else { 0 },
        }
    }

    /// The ring's records, oldest first.
    pub fn ring(&self) -> &LogRing {
        &self.ring
    }

    /// Hands records of the stream to the ring and to every reader they pass. A reader whose
    /// queue is full loses them, and gets a record that says how many before its next one. A
    /// reader that has gone is removed; returns whether one was, since the guest may then
    /// send less.
    pub fn publish(&mut self, entries: Vec<LogEntry>) -> bool {
        let mut gone: Vec<u64> = Vec::new();
        for entry in entries {
            for reader in &mut self.readers {
                if !reader.filter.passes(&entry) || gone.contains(&reader.id) {
                    continue;
                }
                if reader.dropped > 0 {
                    match reader.queue.try_send(LogEntry::dropped(reader.dropped)) {
                        Ok(()) => reader.dropped = 0,
                        Err(TrySendError::Full(_)) => {
                            reader.dropped += 1;
                            continue;
                        }
                        Err(TrySendError::Disconnected(_)) => {
                            gone.push(reader.id);
                            continue;
                        }
                    }
                }
                match reader.queue.try_send(entry.clone()) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => reader.dropped += 1,
                    Err(TrySendError::Disconnected(_)) => gone.push(reader.id),
                }
            }
            if entry.dropped == 0 {
                self.ring.push(entry);
            }
        }
        // A reader whose receiver was dropped without unsubscribing.
        self.readers.retain(|r| !gone.contains(&r.id));
        !gone.is_empty()
    }

    fn next_request(&mut self) -> u32 {
        self.next_request = self.next_request.wrapping_add(1).max(1);
        self.next_request
    }

    /// Starts a dump; returns its request ID and where its records arrive.
    pub fn begin_dump(&mut self, filter: LogFilter) -> (u32, Receiver<LogDumpEvent>) {
        let id = self.next_request();
        let (tx, rx) = mpsc::channel();
        self.dumps.insert(id, (filter, tx));
        (id, rx)
    }

    /// Forgets a dump nobody waits for any longer.
    pub fn end_dump(&mut self, id: u32) {
        self.dumps.remove(&id);
    }

    /// Hands a dump's records to whoever waits for it. Returns false for a dump nobody waits for.
    pub fn dump_records(&mut self, id: u32, entries: Vec<LogEntry>, end: bool, error: &str) -> bool {
        let Some((filter, tx)) = self.dumps.get(&id) else {
            return false;
        };
        let passing: Vec<LogEntry> = entries.into_iter().filter(|e| filter.passes(e)).collect();
        let mut alive = passing.is_empty() || tx.send(LogDumpEvent::Entries(passing)).is_ok();
        if end || !error.is_empty() {
            let why = Some(error.to_string()).filter(|e| !e.is_empty());
            let _ = tx.send(LogDumpEvent::End(why));
            alive = false;
        }
        if !alive {
            self.dumps.remove(&id);
        }
        true
    }

    /// Starts gathering diagnostics; returns the request ID and where the files arrive.
    pub fn begin_diagnostics(&mut self) -> (u32, Receiver<DiagnosticsEvent>) {
        let id = self.next_request();
        let (tx, rx) = mpsc::channel();
        self.diagnostics.insert(id, tx);
        (id, rx)
    }

    pub fn end_diagnostics(&mut self, id: u32) {
        self.diagnostics.remove(&id);
    }

    /// Hands a piece of the diagnostics to whoever waits for it. Returns false when nobody does.
    pub fn diagnostics_event(&mut self, id: u32, event: DiagnosticsEvent) -> bool {
        let Some(tx) = self.diagnostics.get(&id) else {
            return false;
        };
        let done = matches!(event, DiagnosticsEvent::Done(_));
        if tx.send(event).is_err() || done {
            self.diagnostics.remove(&id);
        }
        true
    }

    /// The guest is gone: the dumps and diagnostics under way end with `why`.
    pub fn guest_gone(&mut self, why: &str) {
        for (_, (_, tx)) in self.dumps.drain() {
            let _ = tx.send(LogDumpEvent::End(Some(why.to_string())));
        }
        for (_, tx) in self.diagnostics.drain() {
            let _ = tx.send(DiagnosticsEvent::Done(Some(why.to_string())));
        }
    }
}

/// A reader's view of the guest's log, from the session: the records the host held when it
/// began, then each new one, until it is dropped. Dropping the last reader stops the guest
/// reading its log.
pub struct LogWatch {
    pub history: Vec<LogEntry>,
    pub entries: Receiver<LogEntry>,
    on_end: Option<Box<dyn FnOnce() + Send>>,
}

impl LogWatch {
    pub fn new(
        history: Vec<LogEntry>,
        entries: Receiver<LogEntry>,
        on_end: impl FnOnce() + Send + 'static,
    ) -> LogWatch {
        LogWatch {
            history,
            entries,
            on_end: Some(Box::new(on_end)),
        }
    }
}

impl Drop for LogWatch {
    fn drop(&mut self) {
        if let Some(on_end) = self.on_end.take() {
            on_end();
        }
    }
}

impl fmt::Debug for LogWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LogWatch")
    }
}

/// What one frame on the log port says, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogPortMessage {
    Records {
        request_id: u32,
        entries: Vec<LogEntry>,
        end: bool,
        error: String,
    },
    Diagnostics {
        request_id: u32,
        event: DiagnosticsEvent,
    },
}

/// Reads one frame of the log port. An error says what is wrong with it; the connection it
/// came on is then closed.
pub fn read_log_frame(frame: &[u8]) -> Result<LogPortMessage, String> {
    let envelope =
        dsa_protocol::decode(frame).map_err(|e| format!("an unreadable frame ({e})"))?;
    match envelope.body {
        Some(Body::LogRecords(records)) => {
            if records.records.len() > MAX_RECORDS_PER_FRAME {
                return Err(format!(
                    "a frame of {} records, more than the {MAX_RECORDS_PER_FRAME} allowed",
                    records.records.len()
                ));
            }
            let entries = records
                .records
                .into_iter()
                .map(LogEntry::from_control)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(LogPortMessage::Records {
                request_id: records.request_id,
                entries,
                end: records.end,
                error: records.error.chars().take(1024).collect(),
            })
        }
        Some(Body::DiagnosticsFile(file)) => {
            check_archive_name(&file.name)?;
            Ok(LogPortMessage::Diagnostics {
                request_id: file.request_id,
                event: DiagnosticsEvent::File {
                    name: file.name,
                    data: file.data,
                    last: file.last,
                },
            })
        }
        Some(Body::DiagnosticsDone(done)) => Ok(LogPortMessage::Diagnostics {
            request_id: done.request_id,
            event: DiagnosticsEvent::Done(
                Some(done.error.chars().take(4096).collect::<String>()).filter(|e| !e.is_empty()),
            ),
        }),
        Some(_) => Err("a message that does not belong on the log port".into()),
        None => Err("a message of a kind this host does not know".into()),
    }
}

/// Whether `name` may be a path in the diagnostics archive the guest fills: relative, under
/// `android/`, no `..`, no backslash and no control characters, so a hostile guest cannot
/// name a file outside the archive's folder when it is unpacked.
pub fn check_archive_name(name: &str) -> Result<(), String> {
    let bad = name.len() > 255
        || !name.starts_with("android/")
        || name.ends_with('/')
        || name.contains('\\')
        || name.contains(':')
        || name.chars().any(char::is_control)
        || name.split('/').any(|part| part.is_empty() || part == "." || part == "..");
    if bad {
        return Err(format!("{name:?} is not a name the guest may give a diagnostics file"));
    }
    Ok(())
}

/// How long one wait on the log port lasts before it looks whether it should stop.
const PORT_POLL: Duration = Duration::from_millis(200);

/// Serves one authenticated connection on the log port until it ends, `stop` says so, or the
/// guest breaks the rules: then only this connection is closed, and the error says why. Every
/// message that checks out goes to `deliver`.
pub fn serve_log_connection(
    connection: &mut dyn Connection,
    stop: &dyn Fn() -> bool,
    deliver: &mut dyn FnMut(LogPortMessage),
) -> Result<(), String> {
    loop {
        if stop() {
            connection.close();
            return Ok(());
        }
        match connection.recv(Some(PORT_POLL)) {
            Ok(frame) => match read_log_frame(&frame) {
                Ok(message) => deliver(message),
                Err(why) => {
                    connection.close();
                    return Err(why);
                }
            },
            Err(TransportError::Timeout) => {}
            Err(TransportError::Closed) => return Ok(()),
            Err(error) => return Err(error.to_string()),
        }
    }
}
