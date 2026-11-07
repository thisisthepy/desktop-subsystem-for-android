import XCTest
@testable import DsaSettingsCore

/// The Google Play item on the Advanced page, through `dsa gapps` as the other settings apps
/// run it.
@MainActor
final class GappsViewModelTests: XCTestCase {
    private let installed = #"""
    {"package":{"variant":"core","version":"20261001","android_version":"16.0","sdk":36,"arch":"arm64",
    "file":"LiteGapps-core-arm64-16.0-20261001-official.zip","source":"https://example.com/core.zip","md5":"00"},
    "installed":true,"android":{"installed":true,"active":true,"sdk":36,"abi":"arm64","abilist":"arm64-v8a",
    "device_id":"3812345678901234567"},"restart_needed":false,
    "terms_url":"https://policies.google.com/terms","registration_url":"https://www.google.com/android/uncertified/"}
    """#

    private let missing = #"{"package":null,"installed":false,"android":null,"restart_needed":false}"#

    private func make() -> (FakeCli, GappsViewModel) {
        let cli = FakeCli()
        return (cli, GappsViewModel(service: DsaService(cli: cli), text: KeyText()))
    }

    func test_fr32_gapps_status_is_read_even_without_the_newer_fields() {
        let info = DsaService.readGapps(try! Json.parse(#"{"package":{"version":"20261001"}}"#))
        XCTAssertEqual(info.package?.version, "20261001")
        XCTAssertEqual(info.package?.arch, "")
        XCTAssertEqual(info.package?.sdk, 0)
        XCTAssertNil(info.android)
        XCTAssertFalse(info.restartNeeded)
        XCTAssertEqual(info.termsUrl, "https://policies.google.com/terms")
        XCTAssertEqual(info.registrationUrl, "https://www.google.com/android/uncertified/")

        let full = DsaService.readGapps(try! Json.parse(installed))
        XCTAssertEqual(full.package, GappsPackageInfo(
            variant: "core", version: "20261001", androidVersion: "16.0", sdk: 36, arch: "arm64",
            file: "LiteGapps-core-arm64-16.0-20261001-official.zip", source: "https://example.com/core.zip"))
        XCTAssertEqual(full.android?.deviceId, "3812345678901234567")
        XCTAssertEqual(full.android?.abilist, "arm64-v8a")
        XCTAssertTrue(full.android?.active ?? false)
    }

    func test_fr32_before_install_the_item_offers_install_only() async {
        let (cli, vm) = make()
        XCTAssertFalse(vm.canInstall, "nothing is offered before the service has answered")
        XCTAssertEqual(vm.statusText, "Gapps_Unknown")
        cli.answer("gapps status --json", missing)
        await vm.load()
        XCTAssertEqual(vm.state, .notInstalled)
        XCTAssertEqual(vm.statusText, "Gapps_NotInstalled")
        XCTAssertTrue(vm.canInstall)
        XCTAssertFalse(vm.canRemove)
        XCTAssertFalse(vm.restartNeeded)
        XCTAssertFalse(vm.hasDeviceId)
    }

    func test_fr32_after_install_the_item_names_the_package_and_offers_remove() async {
        let (cli, vm) = make()
        cli.answer("gapps status --json", installed)
        await vm.load()
        XCTAssertEqual(vm.state, .installed)
        XCTAssertEqual(vm.statusText, "Gapps_Installed")
        XCTAssertFalse(vm.canInstall)
        XCTAssertTrue(vm.canRemove)
        XCTAssertTrue(vm.hasDeviceId)
        XCTAssertEqual(vm.deviceId, "3812345678901234567")
    }

    func test_fr32_a_change_android_has_not_started_with_waits_for_a_restart() async {
        let (cli, vm) = make()
        cli.answer("gapps status --json", missing)
        cli.answer("gapps install --json", #"""
        {"package":{"version":"20261001","android_version":"16.0","arch":"x86_64","file":"core.zip"},
        "installed":true,"android":{"installed":true,"active":false,"sdk":36,"abi":"x86_64","abilist":"x86_64"},
        "restart_needed":true}
        """#)
        await vm.load()
        await vm.install()
        XCTAssertTrue(cli.called("gapps install --json"))
        XCTAssertEqual(vm.state, .restartPending)
        XCTAssertEqual(vm.statusText, "Gapps_InstalledRestart")
        XCTAssertEqual(vm.packageText, "Gapps_Installed")
        XCTAssertTrue(vm.restartNeeded)
        XCTAssertTrue(vm.canRemove)
        XCTAssertFalse(vm.isBusy)
        XCTAssertFalse(vm.isInstalling)

        cli.answer("gapps remove --json", #"""
        {"package":null,"installed":false,"android":{"installed":false,"active":true},"restart_needed":true}
        """#)
        await vm.remove()
        XCTAssertTrue(cli.called("gapps remove --json"))
        XCTAssertEqual(vm.state, .restartPending)
        XCTAssertEqual(vm.statusText, "Gapps_RemovedRestart")
        XCTAssertTrue(vm.canInstall)
        XCTAssertFalse(vm.canRemove)
    }

    func test_fr32_the_buttons_run_only_what_the_state_allows() async {
        let (cli, vm) = make()
        cli.answer("gapps status --json", installed)
        await vm.load()
        await vm.install()
        XCTAssertFalse(cli.called("gapps install --json"), "a package is installed already")
        cli.answer("gapps status --json", missing)
        await vm.load()
        await vm.remove()
        XCTAssertFalse(cli.called("gapps remove --json"), "nothing is installed")
    }

    func test_fr32_a_failed_install_shows_why_and_keeps_the_state() async {
        let (cli, vm) = make()
        cli.answer("gapps status --json", missing)
        cli.fail("gapps install --json", "dsa: the package is for arm64, but this Android is x86_64")
        await vm.load()
        await vm.install()
        XCTAssertEqual(vm.error, "Problem_Refused (the package is for arm64, but this Android is x86_64)")
        XCTAssertEqual(vm.state, .notInstalled)
        XCTAssertTrue(vm.canInstall)
    }
}
