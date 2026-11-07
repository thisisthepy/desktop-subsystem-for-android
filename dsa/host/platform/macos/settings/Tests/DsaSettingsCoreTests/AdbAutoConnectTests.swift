import XCTest
@testable import DsaSettingsCore

/// The Advanced page's automatic adb connection: its switch, on by default, and the state the
/// service reports.
@MainActor
final class AdbAutoConnectTests: XCTestCase {
    private let off = #"{"developer_mode":false,"adb_port":58526,"adb_auto_connect":true,"adb_auto_connect_state":"off"}"#

    private func make() -> (FakeCli, DsaService) {
        let cli = FakeCli()
        return (cli, DsaService(cli: cli))
    }

    func test_fr20_auto_connect_is_on_by_default_and_the_switch_turns_it_off() async {
        let (cli, service) = make()
        cli.answer("settings --json", off)
        cli.answer("settings set adb-auto-connect off --json", off.replacingOccurrences(of: #""adb_auto_connect":true"#, with: #""adb_auto_connect":false"#))
        cli.answer("settings set adb-auto-connect on --json", off)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertTrue(settings.adbAutoConnect)
        XCTAssertEqual(settings.adbAutoConnectState, .off)
        XCTAssertEqual(settings.adbAutoConnectStateText, "AdbAutoConnect_Off")

        await settings.setAdbAutoConnect(false)
        XCTAssertFalse(settings.adbAutoConnect)
        await settings.setAdbAutoConnect(true)
        XCTAssertTrue(settings.adbAutoConnect)
        XCTAssertTrue(cli.calls.contains(["settings", "set", "adb-auto-connect", "off", "--json"]))
        XCTAssertTrue(cli.calls.contains(["settings", "set", "adb-auto-connect", "on", "--json"]))
    }

    func test_fr20_a_service_that_predates_auto_connect_reads_as_on_and_off() async {
        let (cli, service) = make()
        cli.answer("settings --json", #"{"developer_mode":false,"adb_port":58526}"#)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertTrue(settings.adbAutoConnect)
        XCTAssertEqual(settings.adbAutoConnectState, .off)
    }

    func test_fr20_each_state_name_is_read() {
        XCTAssertEqual(AdbAutoConnectState.read("off"), .off)
        XCTAssertEqual(AdbAutoConnectState.read("waiting-for-server"), .waitingForServer)
        XCTAssertEqual(AdbAutoConnectState.read("waiting-for-android"), .waitingForAndroid)
        XCTAssertEqual(AdbAutoConnectState.read("attached"), .attached)
        XCTAssertEqual(AdbAutoConnectState.read("detached"), .detached)
        XCTAssertEqual(AdbAutoConnectState.read("something-new"), .off)
        XCTAssertEqual(AdbAutoConnectState.read(nil), .off)
        XCTAssertEqual(
            AdbAutoConnectState.allCases.map(\.textKey),
            ["AdbAutoConnect_Off", "AdbAutoConnect_WaitingForServer", "AdbAutoConnect_WaitingForAndroid",
             "AdbAutoConnect_Attached", "AdbAutoConnect_Detached"])
    }

    func test_fr20_the_state_follows_the_service_while_developer_mode_is_on() async {
        let (cli, service) = make()
        let on = off.replacingOccurrences(of: #""developer_mode":false"#, with: #""developer_mode":true"#)
            .replacingOccurrences(of: #""adb_auto_connect_state":"off""#, with: #""adb_auto_connect_state":"waiting-for-server""#)
        cli.answer("settings --json", on)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertEqual(settings.adbAutoConnectState, .waitingForServer)
        cli.answer("settings --json", on.replacingOccurrences(of: "waiting-for-server", with: "attached"))
        await settings.pollAdbAutoConnect()
        XCTAssertEqual(settings.adbAutoConnectState, .attached)
        XCTAssertEqual(settings.adbAutoConnectStateText, "AdbAutoConnect_Attached")
    }

    func test_fr20_nothing_is_read_again_while_developer_mode_is_off() async {
        let (cli, service) = make()
        cli.answer("settings --json", off)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        let before = cli.calls.filter { $0 == ["settings", "--json"] }.count
        await settings.pollAdbAutoConnect()
        XCTAssertEqual(cli.calls.filter { $0 == ["settings", "--json"] }.count, before)
    }
}
