import XCTest
@testable import DsaSettingsCore

/// The protected content item on the Advanced page, through `dsa drm widevine` as the other
/// settings apps run it.
@MainActor
final class WidevineViewModelTests: XCTestCase {
    private let pending = #"""
    {"package":{"apex":"com.google.android.widevine.apex","version":"BE2A.250530.026.F3 (API 36, r7)",
    "android_major":16,"abi":"arm64-v8a","source":"https://dl.google.com/x.zip","size":4096,"sha256":"00",
    "license":"android-sdk-license"},"installed":true,"on_disk":true,"running":true,"active":false,
    "restart_needed":true,"security_level":"L3","terms_url":"https://developer.android.com/studio/terms"}
    """#

    private let missing = #"""
    {"package":null,"installed":false,"on_disk":false,"running":true,"active":false,"restart_needed":false,
    "security_level":"L3"}
    """#

    private let removed = #"""
    {"package":null,"installed":false,"on_disk":false,"running":true,"active":true,"restart_needed":true,
    "security_level":"L3"}
    """#

    private func make() -> (FakeCli, WidevineViewModel) {
        let cli = FakeCli()
        return (cli, WidevineViewModel(service: DsaService(cli: cli), text: KeyText()))
    }

    func test_fr37_before_install_the_item_offers_install_only() async {
        let (cli, vm) = make()
        XCTAssertFalse(vm.canInstall, "nothing is offered before the service has answered")
        XCTAssertEqual(vm.statusText, "Widevine_Unknown")
        cli.answer("drm widevine status --json", missing)
        await vm.load()
        XCTAssertEqual(vm.state, .notInstalled)
        XCTAssertEqual(vm.statusText, "Widevine_NotInstalled")
        XCTAssertTrue(vm.canInstall)
        XCTAssertFalse(vm.canRemove)
        XCTAssertEqual(vm.qualityText, "Widevine_Quality")
        XCTAssertEqual(vm.termsUrl, "https://developer.android.com/studio/terms")
    }

    func test_fr37_install_runs_the_cli_with_the_users_agreement_and_waits_for_a_restart() async {
        let (cli, vm) = make()
        cli.answer("drm widevine status --json", missing)
        cli.answer("drm widevine install --yes --json", pending)
        await vm.load()
        await vm.install()
        XCTAssertTrue(cli.calls.contains(["drm", "widevine", "install", "--yes", "--json"]))
        XCTAssertNil(vm.error)
        XCTAssertEqual(vm.state, .restartPending)
        XCTAssertEqual(vm.statusText, "Widevine_InstalledRestart")
        XCTAssertEqual(vm.info?.package?.abi, "arm64-v8a")
        XCTAssertEqual(vm.info?.package?.androidMajor, 16)
        XCTAssertFalse(vm.canInstall)
        XCTAssertTrue(vm.canRemove)
    }

    func test_fr37_remove_runs_the_cli_and_waits_for_a_restart() async {
        let (cli, vm) = make()
        cli.answer("drm widevine status --json", pending)
        cli.answer("drm widevine remove --json", removed)
        await vm.load()
        await vm.remove()
        XCTAssertTrue(cli.calls.contains(["drm", "widevine", "remove", "--json"]))
        XCTAssertEqual(vm.state, .restartPending)
        XCTAssertEqual(vm.statusText, "Widevine_RemovedRestart")
        XCTAssertTrue(vm.canInstall)
    }

    func test_fr37_a_refused_install_shows_why_and_keeps_the_state() async {
        let (cli, vm) = make()
        cli.answer("drm widevine status --json", missing)
        cli.fail("drm widevine install --yes --json", "dsa: Google's image holds no Widevine module; nothing was changed")
        await vm.load()
        await vm.install()
        XCTAssertNotNil(vm.error)
        XCTAssertEqual(vm.state, .notInstalled)
        XCTAssertTrue(vm.canInstall)
    }
}
