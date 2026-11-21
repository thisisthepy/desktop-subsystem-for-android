import XCTest
@testable import DsaSettingsCore

/// A dsa whose `status --json` answers with what the test sets, and whose restart waits until
/// the test lets it finish, so the test sees the button while the restart runs.
final class HeldRestartCli: DsaCli {
    static let restart = "android restart --wait --timeout 360 --json"
    var status = ""
    private(set) var calls: [String] = []
    private var held: CheckedContinuation<Void, Never>?
    var isHolding: Bool { held != nil }

    func release() {
        held?.resume()
        held = nil
    }

    func run(_ args: [String]) async throws -> CliResult {
        let line = args.joined(separator: " ")
        calls.append(line)
        switch line {
        case "status --json":
            return CliResult(exitCode: 0, output: status, error: "")
        case HeldRestartCli.restart:
            await withCheckedContinuation { held = $0 }
            return CliResult(exitCode: 0, output: #"{"state":"Active","start_button":"restart","restarting":null}"#, error: "")
        default:
            return CliResult(exitCode: 2, output: "", error: "dsa: the test has no answer for \(line)")
        }
    }
}

/// The status card's first button: Start while Android is stopped, Restart while it runs, as
/// the service decides it.
@MainActor
final class StartButtonTests: XCTestCase {
    func test_fr17_the_start_button_becomes_restart_while_android_runs() async {
        let cli = HeldRestartCli()
        let vm = GeneralViewModel(service: DsaService(cli: cli), text: KeyText())
        cli.status = #"{"state":"Stopped","start_button":"start"}"#
        await vm.refresh()
        XCTAssertEqual(vm.startButton, .start)
        XCTAssertEqual(vm.startButtonText, "StartButton.Content")
        XCTAssertTrue(vm.canPressStart)
        XCTAssertFalse(vm.startConfirms)
        XCTAssertFalse(vm.showsProgress)

        for state in ["Active", "Dozing", "Frozen", "Stalled"] {
            cli.status = #"{"state":"\#(state)","start_button":"restart"}"#
            await vm.refresh()
            XCTAssertEqual(vm.startButton, .restart, state)
            XCTAssertEqual(vm.startButtonText, "RestartButton.Content", state)
            XCTAssertTrue(vm.canPressStart, state)
            XCTAssertTrue(vm.startConfirms, state)
            XCTAssertFalse(vm.canStart, state)
        }

        // A start another program made: locked, with progress.
        cli.status = #"{"state":"Starting","start_button":"starting"}"#
        await vm.refresh()
        XCTAssertFalse(vm.canPressStart)
        XCTAssertTrue(vm.showsProgress)
        XCTAssertEqual(vm.startButtonText, "StartButton.Content")

        // Pressing Restart runs dsa android restart and keeps the button locked until it ends.
        cli.status = #"{"state":"Active","start_button":"restart"}"#
        await vm.refresh()
        let pressing = Task { await vm.pressStart() }
        for _ in 0..<1000 where !cli.isHolding {
            await Task.yield()
        }
        XCTAssertTrue(cli.isHolding)
        XCTAssertEqual(vm.startButton, .restarting)
        XCTAssertEqual(vm.startButtonText, "RestartButton.Content")
        XCTAssertFalse(vm.canPressStart)
        XCTAssertTrue(vm.showsProgress)
        await vm.pressStart()
        XCTAssertEqual(cli.calls.filter { $0 == HeldRestartCli.restart }.count, 1)
        cli.release()
        await pressing.value
        XCTAssertEqual(vm.startButton, .restart)
        XCTAssertTrue(vm.canPressStart)
        XCTAssertFalse(vm.showsProgress)

        // A service that does not say leaves the button locked.
        XCTAssertEqual(StartButtonKind.parse(nil), StartButtonKind.locked)
        XCTAssertEqual(StartButtonKind.parse("later"), StartButtonKind.locked)
    }
}
