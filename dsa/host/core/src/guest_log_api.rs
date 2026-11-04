//! The guest log and the diagnostics on the local API: the service's side of `WatchLog` and
//! `CollectDiagnostics`, and the readers a client gets from
//! [`HostApiClient::watch_log`](crate::host_api::HostApiClient::watch_log) and
//! [`HostApiClient::collect_diagnostics`](crate::host_api::HostApiClient::collect_diagnostics).
//!
//! Each runs on a connection of its own, which carries nothing else once it has begun. A
//! caller only ever reads its own Android's log: the service picks the Android by the user
//! the operating system reports for the connection.

use std::io::{self, Write};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use dsa_protocol::host_api::types::{self, envelope::Body, response, ErrorCode, Envelope, Response};
use dsa_protocol::host_api::{decode, send};
use dsa_transport::{Connection, TransportError};

use crate::diagnostics::{write_archive, HostFile};
use crate::guest_log::{LogDumpEvent, LogEntry, LogFilter};
use crate::host_api::{ClientError, UserAndroid};
use crate::tasks::TaskRequestError;

/// The most records one frame to a client carries.
const ENTRIES_PER_FRAME: usize = 512;

/// How long a connection that reads the log waits for records before it looks whether its
/// client is still there.
const LOG_POLL: Duration = Duration::from_millis(100);

/// How long a dump waits for Android's next records before it gives up.
const DUMP_WAIT: Duration = Duration::from_secs(30);

/// The size of each piece of the diagnostics archive sent to a client.
const ARCHIVE_PIECE: usize = 256 * 1024;

fn answer(connection: &mut dyn Connection, id: u64, body: response::Body) -> bool {
    send(connection, Body::Response(Response { id, body: Some(body) })).is_ok()
}

fn error(code: ErrorCode, detail: String) -> response::Body {
    response::Body::Error(types::Error {
        code: code as i32,
        detail,
    })
}

fn request_error(why: TaskRequestError) -> response::Body {
    match why {
        TaskRequestError::NotRunning(why) => error(ErrorCode::NotRunning, why),
        TaskRequestError::Failed(why) => error(ErrorCode::Unavailable, why),
    }
}

fn send_entries(
    connection: &mut dyn Connection,
    entries: &[LogEntry],
    end: bool,
    problem: Option<&str>,
) -> bool {
    let count = entries.len().div_ceil(ENTRIES_PER_FRAME);
    if count == 0 {
        return send(
            connection,
            Body::LogEntries(types::LogEntries {
                entries: Vec::new(),
                end,
                error: problem.unwrap_or_default().to_string(),
            }),
        )
        .is_ok();
    }
    for (i, chunk) in entries.chunks(ENTRIES_PER_FRAME).enumerate() {
        let last = i + 1 == count;
        let frame = types::LogEntries {
            entries: chunk.iter().map(LogEntry::to_host_api).collect(),
            end: end && last,
            error: if last {
                problem.unwrap_or_default().to_string()
            } else {
                String::new()
            },
        };
        if send(connection, Body::LogEntries(frame)).is_err() {
            return false;
        }
    }
    true
}

/// Whether the client has gone away, or broke the rules by sending something on a connection
/// that only receives.
fn client_gone(connection: &mut dyn Connection) -> bool {
    match connection.recv(Some(Duration::from_millis(1))) {
        Err(TransportError::Timeout) => false,
        Err(_) => true,
        Ok(_) => {
            eprintln!("local API: a client sent a frame on a connection that reads the log");
            connection.close();
            true
        }
    }
}

/// Serves a `WatchLog` request on its own connection.
pub fn serve_watch_log(
    mut connection: Box<dyn Connection>,
    android: Result<Arc<dyn UserAndroid>, String>,
    id: u64,
    watch: &types::WatchLog,
) {
    let android = match android {
        Ok(android) => android,
        Err(why) => {
            answer(&mut *connection, id, error(ErrorCode::Unavailable, why));
            return;
        }
    };
    let filter = match LogFilter::from_host_api(watch) {
        Ok(filter) => filter,
        Err(why) => {
            answer(&mut *connection, id, error(ErrorCode::UnknownRequest, why));
            return;
        }
    };
    if watch.dump {
        return serve_dump(connection, android, id, filter);
    }
    let Some(log) = android.watch_log(filter) else {
        answer(
            &mut *connection,
            id,
            error(
                ErrorCode::Unavailable,
                "this Android cannot send its log".into(),
            ),
        );
        return;
    };
    if !answer(&mut *connection, id, response::Body::Done(types::Done {})) {
        return;
    }
    if !send_entries(&mut *connection, &log.history, false, None) {
        return;
    }
    loop {
        match log.entries.recv_timeout(LOG_POLL) {
            Ok(entry) => {
                let mut batch = vec![entry];
                batch.extend(log.entries.try_iter().take(ENTRIES_PER_FRAME * 4));
                if !send_entries(&mut *connection, &batch, false, None) {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                connection.close();
                return;
            }
        }
        if client_gone(&mut *connection) {
            return;
        }
    }
}

fn serve_dump(
    mut connection: Box<dyn Connection>,
    android: Arc<dyn UserAndroid>,
    id: u64,
    filter: LogFilter,
) {
    let events = match android.dump_log(filter) {
        Ok(events) => events,
        Err(why) => {
            answer(&mut *connection, id, request_error(why));
            return;
        }
    };
    if !answer(&mut *connection, id, response::Body::Done(types::Done {})) {
        return;
    }
    loop {
        match events.recv_timeout(DUMP_WAIT) {
            Ok(LogDumpEvent::Entries(entries)) => {
                if !send_entries(&mut *connection, &entries, false, None) {
                    return;
                }
            }
            Ok(LogDumpEvent::End(problem)) => {
                send_entries(&mut *connection, &[], true, problem.as_deref());
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                send_entries(
                    &mut *connection,
                    &[],
                    true,
                    Some("Android stopped sending its log before the end"),
                );
                return;
            }
            Err(RecvTimeoutError::Disconnected) => {
                send_entries(&mut *connection, &[], true, Some("Android stopped"));
                return;
            }
        }
    }
}

/// Sends the archive to a client as it is written, in pieces.
struct ArchiveSink<'a> {
    connection: &'a mut dyn Connection,
    buffer: Vec<u8>,
}

impl ArchiveSink<'_> {
    fn flush_piece(&mut self, end: bool) -> io::Result<()> {
        let data = std::mem::take(&mut self.buffer);
        send(
            &mut *self.connection,
            Body::DiagnosticsData(types::DiagnosticsData {
                data,
                end,
                error: String::new(),
            }),
        )
        .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e.to_string()))
    }
}

impl Write for ArchiveSink<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        while self.buffer.len() >= ARCHIVE_PIECE {
            let rest = self.buffer.split_off(ARCHIVE_PIECE);
            self.flush_piece(false)?;
            self.buffer = rest;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The host's recent records of the guest's log, as text for the archive.
fn recent_log_text(entries: &[LogEntry]) -> Vec<u8> {
    let mut text = String::new();
    for entry in entries {
        text.push_str(&entry.threadtime());
        text.push('\n');
    }
    text.into_bytes()
}

/// Serves a `CollectDiagnostics` request on its own connection: the guest's part, when its
/// Android runs, and the host's logs `host_files` names, as one zip archive.
pub fn serve_collect_diagnostics(
    mut connection: Box<dyn Connection>,
    android: Result<Arc<dyn UserAndroid>, String>,
    id: u64,
    host_files: &[HostFile],
    build: &str,
) {
    let android = match android {
        Ok(android) => android,
        Err(why) => {
            answer(&mut *connection, id, error(ErrorCode::Unavailable, why));
            return;
        }
    };
    if !answer(&mut *connection, id, response::Body::Done(types::Done {})) {
        return;
    }
    let started = Instant::now();
    let (guest, missing) = match android.collect_diagnostics() {
        Ok(events) => (Some(events), None),
        Err(TaskRequestError::NotRunning(why) | TaskRequestError::Failed(why)) => {
            (None, Some(why))
        }
    };
    let recent = android.recent_log();
    let extra = if recent.is_empty() {
        Vec::new()
    } else {
        vec![("host/android-recent.log".to_string(), recent_log_text(&recent))]
    };
    let about = format!(
        "DSA diagnostics, made by {build} for the Android of the user who asked, with the logs \
         of DSA on this computer."
    );
    let mut sink = ArchiveSink {
        connection: &mut *connection,
        buffer: Vec::new(),
    };
    let written = write_archive(
        &mut sink,
        &about,
        guest,
        missing,
        extra,
        host_files,
        SystemTime::now(),
    );
    match written {
        Ok((sink, report)) => {
            if sink.flush_piece(true).is_ok() {
                eprintln!(
                    "diagnostics: sent an archive of {} files in {:.1} s{}",
                    report.files.len(),
                    started.elapsed().as_secs_f64(),
                    if report.problems.is_empty() {
                        String::new()
                    } else {
                        format!("; not gathered: {}", report.problems.join("; "))
                    }
                );
            }
        }
        Err(why) => {
            let _ = send(
                &mut *connection,
                Body::DiagnosticsData(types::DiagnosticsData {
                    data: Vec::new(),
                    end: true,
                    error: format!("the archive could not be made: {why}"),
                }),
            );
        }
    }
}

/// What a client reads from a connection that reads the guest log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogBatch {
    pub entries: Vec<LogEntry>,
    /// The last frame of a dump.
    pub end: bool,
    /// Why the log could not be read, or why a dump ended early.
    pub error: Option<String>,
}

/// The guest log of the caller's Android, from
/// [`HostApiClient::watch_log`](crate::host_api::HostApiClient::watch_log).
pub struct LogReader {
    connection: Box<dyn Connection>,
}

impl LogReader {
    pub(crate) fn new(connection: Box<dyn Connection>) -> LogReader {
        LogReader { connection }
    }

    /// The next records, waiting at most `timeout`. `Ok(None)` means none came in time; an
    /// error means the connection has ended.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<LogBatch>, ClientError> {
        let frame = match self.connection.recv(Some(timeout)) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => return Ok(None),
            Err(error) => return Err(ClientError::Transport(error)),
        };
        match decode(&frame) {
            Ok(Envelope {
                body: Some(Body::LogEntries(entries)),
            }) => {
                let records = entries
                    .entries
                    .into_iter()
                    .map(LogEntry::from_host_api)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(ClientError::Malformed)?;
                Ok(Some(LogBatch {
                    entries: records,
                    end: entries.end,
                    error: Some(entries.error).filter(|e| !e.is_empty()),
                }))
            }
            Ok(_) => Err(ClientError::Malformed("expected records of the log".into())),
            Err(error) => Err(ClientError::Malformed(error.to_string())),
        }
    }
}

/// The diagnostics archive, from
/// [`HostApiClient::collect_diagnostics`](crate::host_api::HostApiClient::collect_diagnostics).
pub struct DiagnosticsReader {
    connection: Box<dyn Connection>,
}

impl DiagnosticsReader {
    pub(crate) fn new(connection: Box<dyn Connection>) -> DiagnosticsReader {
        DiagnosticsReader { connection }
    }

    /// Writes the whole archive to `out`, waiting at most `idle` for each piece. Returns the
    /// archive's size.
    pub fn save(&mut self, out: &mut dyn Write, idle: Duration) -> Result<u64, ClientError> {
        let mut size = 0u64;
        loop {
            let frame = self
                .connection
                .recv(Some(idle))
                .map_err(ClientError::Transport)?;
            match decode(&frame) {
                Ok(Envelope {
                    body: Some(Body::DiagnosticsData(piece)),
                }) => {
                    if !piece.error.is_empty() {
                        return Err(ClientError::Service {
                            code: ErrorCode::Unavailable,
                            detail: piece.error,
                        });
                    }
                    out.write_all(&piece.data).map_err(|e| {
                        ClientError::Malformed(format!("the archive could not be written: {e}"))
                    })?;
                    size += piece.data.len() as u64;
                    if piece.end {
                        out.flush().map_err(|e| {
                            ClientError::Malformed(format!("the archive could not be written: {e}"))
                        })?;
                        return Ok(size);
                    }
                }
                Ok(_) => {
                    return Err(ClientError::Malformed(
                        "expected a piece of the diagnostics".into(),
                    ))
                }
                Err(error) => return Err(ClientError::Malformed(error.to_string())),
            }
        }
    }
}
