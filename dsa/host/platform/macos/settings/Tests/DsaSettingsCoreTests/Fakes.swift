import Foundation
@testable import DsaSettingsCore

/// A dsa that answers from a table instead of reaching a service, and remembers every command
/// it was given.
final class FakeCli: DsaCli, LogStream {
    var answers: [String: CliResult] = [:]
    var streams: [String: [String]] = [:]
    private(set) var calls: [[String]] = []

    /// Answers `args` (joined by spaces) with `json` printed by a dsa that succeeded.
    func answer(_ args: String, _ json: String) {
        answers[args] = CliResult(exitCode: 0, output: json, error: "")
    }

    /// Answers `args` with a failure, as dsa prints it on standard error.
    func fail(_ args: String, _ error: String) {
        answers[args] = CliResult(exitCode: 1, output: "", error: error)
    }

    func run(_ args: [String]) async throws -> CliResult {
        calls.append(args)
        return answers[args.joined(separator: " ")]
            ?? CliResult(exitCode: 1, output: "", error: "dsa: the fake has no answer for \(args.joined(separator: " "))")
    }

    func lines(_ args: [String]) -> AsyncThrowingStream<String, Error> {
        calls.append(args)
        let lines = streams[args.joined(separator: " ")] ?? []
        return AsyncThrowingStream { continuation in
            for line in lines {
                continuation.yield(line)
            }
            continuation.finish()
        }
    }

    func called(_ args: String) -> Bool {
        calls.contains { $0.joined(separator: " ") == args }
    }
}

/// Gives each text's key back, so the tests read which text a page shows.
final class KeyText: TextSource {
    func get(_ key: String) -> String { key }
}
