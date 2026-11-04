//! The guest's side of the log stream: reads logd only while the host asks, and sends the
//! records on the log port.
//!
//! - The host's `LogStream` turns the stream on, changes what it carries, or turns it off.
//!   While it is off nothing reads logd and nothing is sent, so a guest whose log nobody
//!   reads pays nothing for it.
//! - Records wait in a queue of [`QUEUE`] records between logd and the port. When the host
//!   reads more slowly than Android logs, records are dropped, and a record that says how
//!   many takes their place, in order.
//! - The host's `LogDump` gets what logd holds now, once, on a connection of its own.
//! - The host's `CollectDiagnostics` gets the guest's diagnostics: the whole log, pstore, and
//!   what the agent gathers (which the router diverts here), then `DiagnosticsDone`. Nothing
//!   is written to the guest's disk.
//!
//! The bridge reads logd, other files and the log port through [`LogSource`],
//! [`DiagnosticFiles`] and a [`PortConnector`], so it runs and is tested on any platform.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_protocol::control::{
    envelope::Body, DiagnosticsDone, DiagnosticsFile, Envelope, LogRecord, LogRecords, LogStream,
};
use dsa_transport::Connection;

use crate::logd;

/// The most records waiting between logd and the log port.
pub const QUEUE: usize = 4096;

/// The most records one frame carries.
pub const BATCH: usize = 256;

/// The size of each piece of a diagnostics file.
pub const PIECE: usize = 256 * 1024;

/// How long the bridge waits for the agent's part of the diagnostics.
pub const AGENT_WAIT: Duration = Duration::from_secs(120);

/// How long a dump waits for logd's next entry before it takes the dump to be over.
const DUMP_IDLE: Duration = Duration::from_secs(5);

/// How long one wait for logd or the queue lasts before the bridge looks whether it should
/// stop.
const POLL: Duration = Duration::from_millis(100);

/// Every buffer, as the control protocol numbers them.
const ALL_BUFFERS: u32 = 0b1_1111_1110;

/// Packets from logd, after a request.
pub trait LogPackets: Send {
    /// The next packet, waiting at most `timeout`. `Ok(None)` when none came in time; an empty
    /// packet when logd has closed the reader (a dump is over).
    fn next(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, String>;
}

/// Opens logd's reader with a request line ([`logd::request`]).
pub trait LogSource: Send + Sync {
    fn open(&self, request: &str) -> Result<Box<dyn LogPackets>, String>;
}

/// The files the guest's diagnostics carry besides the log, such as pstore's: each by its
/// name in the archive (under `android/`), with its content or why it could not be read.
pub trait DiagnosticFiles: Send + Sync {
    fn files(&self) -> Vec<(String, Result<Vec<u8>, String>)>;
}

/// Opens a connection on the log port, past the handshake.
pub type PortConnector = dyn Fn() -> Result<Box<dyn Connection>, String> + Send + Sync;

fn frame(body: Body) -> Vec<u8> {
    dsa_protocol::encode(&Envelope { body: Some(body) })
}

/// Records between logd and the port, with the count of those dropped since the last that
/// went in.
struct Queue {
    inner: Mutex<(VecDeque<LogRecord>, u64)>,
    ready: Condvar,
    capacity: usize,
}

impl Queue {
    fn new(capacity: usize) -> Queue {
        Queue {
            inner: Mutex::new((VecDeque::new(), 0)),
            ready: Condvar::new(),
            capacity: capacity.max(2),
        }
    }

    fn push(&self, record: LogRecord) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let (queue, dropped) = &mut *inner;
        let room = self.capacity - queue.len().min(self.capacity);
        let needed = if *dropped > 0 { 2 } else { 1 };
        if room < needed {
            *dropped += 1;
            return;
        }
        if *dropped > 0 {
            queue.push_back(LogRecord {
                dropped: *dropped,
                ..LogRecord::default()
            });
            *dropped = 0;
        }
        queue.push_back(record);
        self.ready.notify_one();
    }

    fn pop_batch(&self, max: usize, timeout: Duration) -> Vec<LogRecord> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let (mut inner, _) = self
            .ready
            .wait_timeout_while(inner, timeout, |(queue, _)| queue.is_empty())
            .unwrap_or_else(|e| e.into_inner());
        let take = inner.0.len().min(max);
        inner.0.drain(..take).collect()
    }
}

struct Running {
    wanted: LogStream,
    stop: Arc<AtomicBool>,
}

/// The guest's side of the log stream.
pub struct LogBridge {
    source: Arc<dyn LogSource>,
    files: Arc<dyn DiagnosticFiles>,
    connect: Arc<PortConnector>,
    agent_connected: Arc<dyn Fn() -> bool + Send + Sync>,
    stream: Mutex<Option<Running>>,
    /// The time of the last record the stream read, so a stream that restarts with another
    /// filter goes on from there instead of starting over.
    last_ns: Arc<AtomicU64>,
    /// The diagnostics under way, each with where the agent's frames for it go.
    collects: Arc<Mutex<HashMap<u32, Sender<Vec<u8>>>>>,
    queue_capacity: usize,
}

impl LogBridge {
    /// A bridge that reads logd through `source`, the other diagnostic files through `files`,
    /// and opens the log port with `connect`. `agent_connected` says whether the agent is
    /// there to gather its part of the diagnostics.
    pub fn new(
        source: Arc<dyn LogSource>,
        files: Arc<dyn DiagnosticFiles>,
        connect: Arc<PortConnector>,
        agent_connected: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> LogBridge {
        LogBridge {
            source,
            files,
            connect,
            agent_connected,
            stream: Mutex::new(None),
            last_ns: Arc::new(AtomicU64::new(0)),
            collects: Arc::new(Mutex::new(HashMap::new())),
            queue_capacity: QUEUE,
        }
    }

    /// The same bridge with a queue of `capacity` records, for tests.
    pub fn with_queue(mut self, capacity: usize) -> LogBridge {
        self.queue_capacity = capacity;
        self
    }

    /// Takes the host's frames that are the bridge's: returns true for those that stop here.
    /// `CollectDiagnostics` returns false after starting, since the agent gathers its part too.
    pub fn from_host(&self, frame: &[u8]) -> bool {
        let Ok(Envelope { body: Some(body) }) = dsa_protocol::decode(frame) else {
            return false;
        };
        match body {
            Body::LogStream(wanted) => {
                self.set_stream(wanted);
                true
            }
            Body::LogDump(dump) => {
                let (source, connect) = (Arc::clone(&self.source), Arc::clone(&self.connect));
                spawn("dsa-log-dump", move || {
                    run_dump(
                        &*source,
                        &*connect,
                        dump.request_id,
                        dump.buffers,
                        dump.min_priority,
                    )
                });
                true
            }
            Body::CollectDiagnostics(collect) => {
                let (agent_tx, agent_rx) = mpsc::channel();
                let agent = (self.agent_connected)();
                if agent {
                    self.collects
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(collect.request_id, agent_tx);
                }
                let (source, files, connect, collects) = (
                    Arc::clone(&self.source),
                    Arc::clone(&self.files),
                    Arc::clone(&self.connect),
                    Arc::clone(&self.collects),
                );
                let request_id = collect.request_id;
                spawn("dsa-diagnostics", move || {
                    run_collect(
                        &*source,
                        &*files,
                        &*connect,
                        request_id,
                        agent.then_some(agent_rx),
                    );
                    collects
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&request_id);
                });
                false
            }
            _ => false,
        }
    }

    /// Where the router sends the agent's diagnostics frames.
    pub fn agent_frames(&self) -> Sender<Vec<u8>> {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let collects = Arc::clone(&self.collects);
        spawn("dsa-diagnostics-agent", move || {
            for frame in rx {
                let request_id = match dsa_protocol::decode(&frame) {
                    Ok(Envelope {
                        body: Some(Body::DiagnosticsFile(file)),
                    }) => file.request_id,
                    Ok(Envelope {
                        body: Some(Body::DiagnosticsDone(done)),
                    }) => done.request_id,
                    _ => continue,
                };
                let collects = collects.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(to) = collects.get(&request_id) {
                    let _ = to.send(frame);
                }
            }
        });
        tx
    }

    /// Whether the stream is on.
    pub fn streaming(&self) -> bool {
        self.stream
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// The host is gone: nobody reads the log any more until a host asks again.
    pub fn host_lost(&self) {
        self.set_stream(LogStream::default());
    }

    fn set_stream(&self, wanted: LogStream) {
        let mut stream = self.stream.lock().unwrap_or_else(|e| e.into_inner());
        let on = wanted.on && wanted.buffers & ALL_BUFFERS != 0;
        if let Some(running) = stream.as_ref() {
            let same = on
                && running.wanted.buffers == wanted.buffers
                && running.wanted.min_priority == wanted.min_priority;
            if same {
                return;
            }
        }
        if let Some(running) = stream.take() {
            running.stop.store(true, Ordering::Relaxed);
        }
        if !on {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (source, connect, last_ns, capacity) = (
            Arc::clone(&self.source),
            Arc::clone(&self.connect),
            Arc::clone(&self.last_ns),
            self.queue_capacity,
        );
        let (run_stop, run_wanted) = (Arc::clone(&stop), wanted);
        spawn("dsa-log-stream", move || {
            run_stream(source, &*connect, run_wanted, run_stop, last_ns, capacity)
        });
        *stream = Some(Running { wanted, stop });
    }
}

fn spawn(name: &str, work: impl FnOnce() + Send + 'static) {
    if let Err(error) = thread::Builder::new().name(name.into()).spawn(work) {
        eprintln!("dsa-channeld: cannot start {name}: {error}");
    }
}

/// Whether a record passes the stream's least priority. Binary buffers have none of their own,
/// so they always do.
fn passes(record: &LogRecord, min_priority: u32) -> bool {
    matches!(record.buffer, 3 | 6 | 7) || record.priority >= min_priority
}

/// Reads logd into `queue` until `stop`, reopening it when it ends.
fn read_logd(
    source: &dyn LogSource,
    wanted: &LogStream,
    stop: &AtomicBool,
    last_ns: &AtomicU64,
    queue: &Queue,
) {
    let mut tail = wanted.tail;
    let mut reported = false;
    while !stop.load(Ordering::Relaxed) {
        let last = last_ns.load(Ordering::Relaxed);
        let start = if last > 0 { last + 1 } else { logd::now_ns() };
        let request = logd::request(wanted.buffers, false, tail, Some(start));
        let mut packets = match source.open(&request) {
            Ok(packets) => packets,
            Err(why) => {
                if !reported {
                    eprintln!("dsa-channeld: cannot read the log: {why}");
                    reported = true;
                }
                pause(Duration::from_secs(1), stop);
                continue;
            }
        };
        tail = 0;
        while !stop.load(Ordering::Relaxed) {
            match packets.next(POLL) {
                Ok(None) => {}
                Ok(Some(packet)) if packet.is_empty() => break,
                Ok(Some(packet)) => match logd::parse_entry(&packet) {
                    Ok(record) => {
                        last_ns.fetch_max(record.time_ns, Ordering::Relaxed);
                        if passes(&record, wanted.min_priority) {
                            queue.push(record);
                        }
                    }
                    Err(why) => eprintln!("dsa-channeld: skipped a log entry: {why}"),
                },
                Err(why) => {
                    eprintln!("dsa-channeld: reading the log failed: {why}");
                    break;
                }
            }
        }
        // logd closed the reader (it does so to one that falls far behind): open it again.
        pause(Duration::from_millis(200), stop);
    }
}

fn pause(delay: Duration, stop: &AtomicBool) {
    let deadline = Instant::now() + delay;
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(20).min(deadline - Instant::now()));
    }
}

fn run_stream(
    source: Arc<dyn LogSource>,
    connect: &PortConnector,
    wanted: LogStream,
    stop: Arc<AtomicBool>,
    last_ns: Arc<AtomicU64>,
    capacity: usize,
) {
    let queue = Arc::new(Queue::new(capacity));
    let reader = {
        let (queue, stop, wanted, last_ns) = (
            Arc::clone(&queue),
            Arc::clone(&stop),
            wanted,
            Arc::clone(&last_ns),
        );
        thread::Builder::new()
            .name("dsa-log-reader".into())
            .spawn(move || read_logd(&*source, &wanted, &stop, &last_ns, &queue))
    };
    let mut connection: Option<Box<dyn Connection>> = None;
    let mut delay = Duration::from_millis(100);
    let mut reported = false;
    while !stop.load(Ordering::Relaxed) {
        if connection.is_none() {
            match connect() {
                Ok(opened) => {
                    connection = Some(opened);
                    delay = Duration::from_millis(100);
                    reported = false;
                }
                Err(why) => {
                    if !reported {
                        eprintln!("dsa-channeld: cannot reach the host's log port: {why}");
                        reported = true;
                    }
                    pause(delay, &stop);
                    delay = (delay * 2).min(Duration::from_secs(5));
                    continue;
                }
            }
        }
        let batch = queue.pop_batch(BATCH, POLL);
        if batch.is_empty() {
            continue;
        }
        let sent = connection.as_mut().map(|c| {
            c.send(&frame(Body::LogRecords(LogRecords {
                request_id: 0,
                records: batch,
                end: false,
                error: String::new(),
            })))
        });
        if let Some(Err(error)) = sent {
            eprintln!("dsa-channeld: the log port was lost ({error}); connecting again");
            connection = None;
        }
    }
    if let Some(mut connection) = connection {
        connection.close();
    }
    if let Ok(reader) = reader {
        let _ = reader.join();
    }
}

/// Reads what logd holds now through `source`, and hands each record that passes to `each`.
/// Returns why it ended early, if it did.
fn read_all(
    source: &dyn LogSource,
    buffers: u32,
    min_priority: u32,
    each: &mut dyn FnMut(LogRecord) -> Result<(), String>,
) -> Option<String> {
    let mut packets = match source.open(&logd::request(buffers, true, 0, None)) {
        Ok(packets) => packets,
        Err(why) => return Some(format!("the log could not be read: {why}")),
    };
    let mut idle = Instant::now();
    loop {
        match packets.next(POLL) {
            Ok(None) => {
                if idle.elapsed() > DUMP_IDLE {
                    return None;
                }
            }
            Ok(Some(packet)) if packet.is_empty() => return None,
            Ok(Some(packet)) => {
                idle = Instant::now();
                if let Ok(record) = logd::parse_entry(&packet) {
                    if passes(&record, min_priority) {
                        if let Err(why) = each(record) {
                            return Some(why);
                        }
                    }
                }
            }
            Err(why) => return Some(format!("reading the log failed: {why}")),
        }
    }
}

fn run_dump(
    source: &dyn LogSource,
    connect: &PortConnector,
    request_id: u32,
    buffers: u32,
    min_priority: u32,
) {
    let mut connection = match connect() {
        Ok(connection) => connection,
        Err(why) => {
            eprintln!("dsa-channeld: cannot send the log dump: {why}");
            return;
        }
    };
    let mut batch = Vec::new();
    let problem = {
        let connection = &mut connection;
        let mut each = |record: LogRecord| -> Result<(), String> {
            batch.push(record);
            if batch.len() >= BATCH {
                send_records(
                    &mut **connection,
                    request_id,
                    &mut batch,
                    false,
                    String::new(),
                )?;
            }
            Ok(())
        };
        read_all(source, buffers, min_priority, &mut each)
    };
    let _ = send_records(
        &mut *connection,
        request_id,
        &mut batch,
        true,
        problem.unwrap_or_default(),
    );
    connection.close();
}

fn send_records(
    connection: &mut dyn Connection,
    request_id: u32,
    batch: &mut Vec<LogRecord>,
    end: bool,
    error: String,
) -> Result<(), String> {
    connection
        .send(&frame(Body::LogRecords(LogRecords {
            request_id,
            records: std::mem::take(batch),
            end,
            error,
        })))
        .map_err(|e| e.to_string())
}

/// Sends one file of the diagnostics in pieces.
fn send_file(
    connection: &mut dyn Connection,
    request_id: u32,
    name: &str,
    data: &[u8],
) -> Result<(), String> {
    let pieces = data.len().div_ceil(PIECE).max(1);
    for i in 0..pieces {
        let piece = &data[(i * PIECE).min(data.len())..((i + 1) * PIECE).min(data.len())];
        connection
            .send(&frame(Body::DiagnosticsFile(DiagnosticsFile {
                request_id,
                name: name.to_string(),
                data: piece.to_vec(),
                last: i + 1 == pieces,
            })))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn run_collect(
    source: &dyn LogSource,
    files: &dyn DiagnosticFiles,
    connect: &PortConnector,
    request_id: u32,
    agent: Option<Receiver<Vec<u8>>>,
) {
    let mut connection = match connect() {
        Ok(connection) => connection,
        Err(why) => {
            eprintln!("dsa-channeld: cannot send the diagnostics: {why}");
            return;
        }
    };
    let mut problems = Vec::new();
    // The whole log, every buffer, as text, sent in pieces as it is read.
    let mut text = Vec::new();
    let name = "android/logcat.txt";
    let problem = {
        let connection = &mut connection;
        let mut each = |record: LogRecord| -> Result<(), String> {
            text.extend_from_slice(logd::threadtime(&record).as_bytes());
            if text.len() >= PIECE {
                let piece = std::mem::take(&mut text);
                connection
                    .send(&frame(Body::DiagnosticsFile(DiagnosticsFile {
                        request_id,
                        name: name.to_string(),
                        data: piece,
                        last: false,
                    })))
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        };
        read_all(source, ALL_BUFFERS, 0, &mut each)
    };
    if let Some(problem) = problem {
        problems.push(problem);
    }
    if send_file(&mut *connection, request_id, name, &text).is_err() {
        return;
    }
    for (name, content) in files.files() {
        match content {
            Ok(data) => {
                if send_file(&mut *connection, request_id, &name, &data).is_err() {
                    return;
                }
            }
            Err(why) => problems.push(format!("{name}: {why}")),
        }
    }
    match agent {
        None => problems.push("the agent is not connected, so its part is missing".into()),
        Some(agent) => loop {
            match agent.recv_timeout(AGENT_WAIT) {
                Ok(agent_frame) => match dsa_protocol::decode(&agent_frame) {
                    Ok(Envelope {
                        body: Some(Body::DiagnosticsDone(done)),
                    }) => {
                        if !done.error.is_empty() {
                            problems.push(format!("the agent: {}", done.error));
                        }
                        break;
                    }
                    Ok(_) => {
                        if connection.send(&agent_frame).is_err() {
                            return;
                        }
                    }
                    Err(_) => {}
                },
                Err(RecvTimeoutError::Timeout) => {
                    problems.push(format!(
                        "the agent sent nothing for {} seconds; its part may be incomplete",
                        AGENT_WAIT.as_secs()
                    ));
                    break;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        },
    }
    let _ = connection.send(&frame(Body::DiagnosticsDone(DiagnosticsDone {
        request_id,
        error: problems.join("; "),
    })));
    connection.close();
}

/// The files of `dir`, each under `prefix` in the archive, for [`DiagnosticFiles`]. A folder
/// that does not exist has none.
pub fn files_in(dir: &std::path::Path, prefix: &str) -> Vec<(String, Result<Vec<u8>, String>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| {
            let name = e
                .file_name()
                .to_string_lossy()
                .replace(['/', '\\', ':'], "_");
            (
                format!("{prefix}/{name}"),
                std::fs::read(e.path()).map_err(|error| error.to_string()),
            )
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}
