import Foundation

/// One record of Android's log, as `dsa logcat --json` prints it.
public struct LogLine: Equatable, Identifiable, Sendable {
    public var id = UUID()
    public var time, priority, buffer, tag: String
    public var pid, tid: Int64
    public var message: String
    public var dropped: UInt64

    public static func == (a: LogLine, b: LogLine) -> Bool {
        a.time == b.time && a.priority == b.priority && a.buffer == b.buffer && a.tag == b.tag
            && a.pid == b.pid && a.tid == b.tid && a.message == b.message && a.dropped == b.dropped
    }

    private static func pad(_ number: Int64) -> String {
        let text = String(number)
        return String(repeating: " ", count: max(0, 5 - text.count)) + text
    }

    /// The record as logcat's threadtime format prints it.
    public var text: String {
        dropped > 0
            ? "--------- \(dropped) records dropped"
            : "\(time) \(LogLine.pad(pid)) \(LogLine.pad(tid)) \(priority) \(tag): \(message)"
    }

    // The page's columns, split as the Windows settings app splits them: time, PID, TID,
    // priority and the body. A record that stands for dropped records has only its body.
    public var timeColumn: String { dropped > 0 ? "" : time }
    public var pidColumn: String { dropped > 0 ? "" : String(pid) }
    public var tidColumn: String { dropped > 0 ? "" : String(tid) }
    public var priorityColumn: String { dropped > 0 ? "" : priority }
    public var bodyColumn: String { dropped > 0 ? text : "\(tag): \(message)" }

    /// Reads one line of `dsa logcat --json`; nil for a line that is not a record.
    public static func parse(_ line: String) -> LogLine? {
        guard let json = try? Json.parse(line), json.isObject else {
            return nil
        }
        return LogLine(time: json.string("time"), priority: json.string("priority"), buffer: json.string("buffer"),
                       tag: json.string("tag"), pid: json.int64("pid"), tid: json.int64("tid"),
                       message: json.string("message"), dropped: json.uint("dropped"))
    }
}

/// The Android log page: Android's log as it comes, filtered by priority and by text, which
/// can be paused, and the button that saves the diagnostics. Android sends its log only while
/// this page reads it; nothing is written to disk but the diagnostics archive the user saves.
@MainActor
public final class LogViewModel: ObservableObject {
    /// The most lines the page keeps. Older ones are dropped.
    public static let maxLines = 5000
    /// How often lines read from Android are shown, in seconds.
    public static let flushInterval = 0.1
    /// The priorities the page offers, least first, as logcat's letters.
    public static let priorities = ["V", "D", "I", "W", "E", "F"]

    private let stream: LogStream
    private let service: DsaService
    private let text: TextSource
    private var all: [LogLine] = []
    private var reading: Task<Void, Never>?
    private var generation = 0

    /// The lines shown: those that pass the filter, as of the last time the page was not
    /// paused.
    @Published public private(set) var lines: [LogLine] = []
    @Published public private(set) var isReading = false
    @Published public private(set) var error: String?
    @Published public private(set) var status: String?
    @Published public private(set) var isSaving = false
    /// Lines that came while paused and are not shown yet.
    @Published public private(set) var waiting = 0

    public init(stream: LogStream, service: DsaService, text: TextSource) {
        self.stream = stream
        self.service = service
        self.text = text
    }

    public var canSave: Bool { !isSaving }
    public var pausedText: String { text.format("Logs_Paused", waiting) }

    /// Shows only lines whose tag or message holds this text, ignoring case.
    @Published public var filter = "" {
        didSet {
            if filter != oldValue && !paused {
                rebuild()
            }
        }
    }

    /// The least priority read, as a logcat letter. Android sends only what passes, so
    /// changing it reads the log again.
    @Published public var priority = "I" {
        didSet {
            if priority != oldValue && LogViewModel.priorities.contains(priority) && isReading {
                start()
            }
        }
    }

    /// While paused the lines shown stay as they are; new ones wait, and show when the page
    /// goes on.
    @Published public var paused = false {
        didSet {
            if paused != oldValue && !paused {
                waiting = 0
                rebuild()
            }
        }
    }

    /// The dsa command line that reads the log at the chosen priority.
    public nonisolated static func arguments(_ priority: String) -> [String] {
        ["logcat", "--json", "*:\(priority)"]
    }

    private func passes(_ line: LogLine) -> Bool {
        filter.isEmpty || line.dropped > 0
            || line.tag.range(of: filter, options: .caseInsensitive) != nil
            || line.message.range(of: filter, options: .caseInsensitive) != nil
    }

    private func rebuild() { lines = all.filter(passes) }

    /// What copying the selected lines puts on the pasteboard: each whole line as the log
    /// prints it, one per line, in the order the page shows them.
    public func copyText(_ selected: Set<LogLine.ID>) -> String {
        lines.filter { selected.contains($0.id) }.map(\.text).joined(separator: "\n")
    }

    /// Takes one line of dsa's output.
    public func add(_ output: String) { addMany([output]) }

    /// Takes several lines of dsa's output and shows them in one change of the list. Android
    /// sends its log in bursts of hundreds of lines; one change for each would lay the page out
    /// hundreds of times a second.
    public func addMany(_ outputs: [String]) {
        let parsed = outputs.compactMap(LogLine.parse)
        if parsed.isEmpty {
            return
        }
        all.append(contentsOf: parsed)
        let trimmed = all.count > LogViewModel.maxLines
        if trimmed {
            all.removeFirst(all.count - LogViewModel.maxLines)
        }
        if paused {
            waiting += parsed.count
            return
        }
        if trimmed || parsed.count > 1 {
            rebuild()
        } else if let last = all.last, passes(last) {
            lines.append(last)
        }
    }

    /// Reads the log until `stop`, starting over when already reading.
    public func start() {
        stop()
        generation += 1
        let mine = generation
        all = []
        waiting = 0
        error = nil
        lines = []
        isReading = true
        let source = stream.lines(LogViewModel.arguments(priority))
        reading = Task { [weak self] in
            // Lines are gathered as they come and shown at most ten times a second, one change
            // of the list each time.
            let batch = LineBatch()
            let pump = Task.detached {
                do {
                    for try await line in source {
                        await batch.add(line)
                    }
                    await batch.finish(nil)
                } catch {
                    await batch.finish(error)
                }
            }
            while !Task.isCancelled {
                let (taken, done, failure) = await batch.take()
                guard let self else {
                    break
                }
                self.addMany(taken)
                if done {
                    if let failure, self.generation == mine {
                        self.error = Format.problem(self.text, failure)
                    }
                    break
                }
                try? await Task.sleep(nanoseconds: UInt64(LogViewModel.flushInterval * 1_000_000_000))
            }
            pump.cancel()
            if let self, self.generation == mine {
                self.isReading = false
            }
        }
    }

    /// Stops reading, so Android stops sending its log.
    public func stop() {
        generation += 1
        reading?.cancel()
        reading = nil
        isReading = false
    }

    /// Saves the diagnostics of Android and of DSA to `path`, as one zip archive.
    public func saveDiagnostics(_ path: String) async {
        isSaving = true
        status = text.get("Logs_Saving")
        error = nil
        do {
            _ = try await service.collectDiagnostics(path)
            status = text.format("Logs_Saved", path)
        } catch {
            status = nil
            self.error = Format.problem(text, error)
        }
        isSaving = false
    }
}

/// The lines read from dsa that the page has not shown yet.
private actor LineBatch {
    private var pending: [String] = []
    private var done = false
    private var failure: Error?

    func add(_ line: String) { pending.append(line) }

    func finish(_ error: Error?) {
        done = true
        failure = error
    }

    func take() -> ([String], Bool, Error?) {
        let taken = pending
        pending = []
        return (taken, done, failure)
    }
}
