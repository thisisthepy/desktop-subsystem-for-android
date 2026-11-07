import Foundation

/// What one run of the dsa command printed.
public struct CliResult: Equatable, Sendable {
    public var exitCode: Int32
    public var output: String
    public var error: String

    public init(exitCode: Int32, output: String, error: String) {
        self.exitCode = exitCode
        self.output = output
        self.error = error
    }
}

/// Runs the dsa command. The app never speaks the local API itself: dsa does, and prints its
/// answer as JSON.
public protocol DsaCli: AnyObject {
    func run(_ args: [String]) async throws -> CliResult
}

/// Runs a dsa command whose output goes on until it is stopped, line by line.
public protocol LogStream: AnyObject {
    /// The lines dsa prints, until it ends or the consumer stops reading, which ends dsa.
    func lines(_ args: [String]) -> AsyncThrowingStream<String, Error>
}

/// What kind of thing went wrong, so the page can say it in the user's language. The detail
/// is what dsa said, in English, for a bug report.
public enum Problem: Equatable, Sendable {
    /// dsa could not be started.
    case cliMissing
    /// dsa could not reach the DSA service.
    case serviceUnreachable
    /// The service refused or failed the request; the detail says why.
    case refused
    /// dsa printed something that is not the expected JSON.
    case unreadable
}

public struct DsaError: Error, Equatable, LocalizedError {
    public var problem: Problem
    public var detail: String

    public init(_ problem: Problem, _ detail: String) {
        self.problem = problem
        self.detail = detail
    }

    public var errorDescription: String? { detail }

    /// What dsa said on standard error when it failed, without its `dsa: ` prefix.
    public static func detail(of result: CliResult) -> String {
        let error = result.error.trimmingCharacters(in: .whitespacesAndNewlines)
        return error.hasPrefix("dsa: ") ? String(error.dropFirst(5)) : error
    }

    /// The error for a run of dsa that ended with a failure.
    public static func failure(_ result: CliResult) -> DsaError {
        let detail = detail(of: result)
        let problem: Problem = detail.contains("cannot reach the DSA service") ? .serviceUnreachable : .refused
        return DsaError(problem, detail.isEmpty ? "dsa ended with code \(result.exitCode)" : detail)
    }
}

/// Runs the dsa program as a child process.
public final class ProcessDsaCli: DsaCli, LogStream, @unchecked Sendable {
    public let path: String
    /// Arguments put before every command, such as `--socket <path>`.
    public let prefix: [String]

    public init(path: String, prefix: [String] = []) {
        self.path = path
        self.prefix = prefix
    }

    /// Where dsa is: `DSA_CLI` when it is set, then beside this program (in DSA.app's
    /// `Contents/MacOS`, where the package puts both), then the usual install places.
    public static func locate(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        programFolder: URL? = Bundle.main.executableURL?.deletingLastPathComponent(),
        exists: (String) -> Bool = { FileManager.default.isExecutableFile(atPath: $0) }
    ) -> String {
        if let fromEnvironment = environment["DSA_CLI"], !fromEnvironment.isEmpty {
            return fromEnvironment
        }
        var candidates: [String] = []
        if let folder = programFolder {
            candidates.append(folder.appendingPathComponent("dsa").path)
        }
        candidates += [
            "/Applications/DSA.app/Contents/MacOS/dsa",
            "/usr/local/bin/dsa",
            "/opt/homebrew/bin/dsa",
        ]
        return candidates.first(where: exists) ?? "dsa"
    }

    private func makeProcess(_ args: [String]) -> Process {
        let process = Process()
        if path.contains("/") {
            process.executableURL = URL(fileURLWithPath: path)
            process.arguments = prefix + args
        } else {
            process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
            process.arguments = [path] + prefix + args
        }
        process.standardInput = FileHandle.nullDevice
        return process
    }

    public func run(_ args: [String]) async throws -> CliResult {
        let process = makeProcess(args)
        let output = Pipe()
        let errorPipe = Pipe()
        process.standardOutput = output
        process.standardError = errorPipe
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                do {
                    try process.run()
                } catch {
                    continuation.resume(throwing: DsaError(.cliMissing, "\(self.path): \(error.localizedDescription)"))
                    return
                }
                // Both pipes are read at once, so a full one never stalls dsa.
                let errorData = DataBox()
                let group = DispatchGroup()
                group.enter()
                DispatchQueue.global(qos: .userInitiated).async {
                    errorData.data = errorPipe.fileHandleForReading.readDataToEndOfFile()
                    group.leave()
                }
                let outputData = output.fileHandleForReading.readDataToEndOfFile()
                group.wait()
                process.waitUntilExit()
                continuation.resume(returning: CliResult(
                    exitCode: process.terminationStatus,
                    output: String(decoding: outputData, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines),
                    error: String(decoding: errorData.data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)))
            }
        }
    }

    public func lines(_ args: [String]) -> AsyncThrowingStream<String, Error> {
        AsyncThrowingStream { continuation in
            let process = makeProcess(args)
            let output = Pipe()
            let errorPipe = Pipe()
            process.standardOutput = output
            process.standardError = errorPipe
            continuation.onTermination = { _ in
                if process.isRunning {
                    process.terminate()
                }
            }
            DispatchQueue.global(qos: .utility).async {
                do {
                    try process.run()
                } catch {
                    continuation.finish(throwing: DsaError(.cliMissing, "\(self.path): \(error.localizedDescription)"))
                    return
                }
                let errorData = DataBox()
                let group = DispatchGroup()
                group.enter()
                DispatchQueue.global(qos: .utility).async {
                    errorData.data = errorPipe.fileHandleForReading.readDataToEndOfFile()
                    group.leave()
                }
                var pending = Data()
                let handle = output.fileHandleForReading
                while true {
                    let chunk = handle.availableData
                    if chunk.isEmpty {
                        break
                    }
                    pending.append(chunk)
                    while let newline = pending.firstIndex(of: 0x0A) {
                        let line = pending[pending.startIndex..<newline]
                        continuation.yield(String(decoding: line, as: UTF8.self))
                        pending.removeSubrange(pending.startIndex...newline)
                    }
                }
                if !pending.isEmpty {
                    continuation.yield(String(decoding: pending, as: UTF8.self))
                }
                group.wait()
                process.waitUntilExit()
                if process.terminationReason == .exit && process.terminationStatus != 0 {
                    let result = CliResult(
                        exitCode: process.terminationStatus, output: "",
                        error: String(decoding: errorData.data, as: UTF8.self))
                    continuation.finish(throwing: DsaError.failure(result))
                } else {
                    continuation.finish()
                }
            }
        }
    }
}

/// Bytes one thread reads while another waits for them.
private final class DataBox: @unchecked Sendable {
    var data = Data()
}
