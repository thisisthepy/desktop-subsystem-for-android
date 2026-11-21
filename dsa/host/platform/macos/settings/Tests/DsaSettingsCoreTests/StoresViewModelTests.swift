import XCTest
@testable import DsaSettingsCore

/// The alternative stores item under Google Play, through `dsa stores` as the other settings
/// apps run it.
@MainActor
final class StoresViewModelTests: XCTestCase {
    private func stores(auroraInstalled: Bool) -> String {
        #"{"stores":[{"id":"onestore","name":"ONE store","package":"com.skt.skaf.A000Z00040","homepage":"https://m.onestore.co.kr/","installed":false,"version":""},"#
            + #"{"id":"aurora","name":"Aurora Store","package":"com.aurora.store","homepage":"https://auroraoss.com/","installed":"#
            + (auroraInstalled ? #"true,"version":"4.8.4"}"# : #"false,"version":""}"#)
            + #",{"id":"fdroid","name":"F-Droid","package":"org.fdroid.fdroid","homepage":"https://f-droid.org/","installed":false,"version":""}]}"#
    }

    private func make() -> (FakeCli, StoresViewModel) {
        let cli = FakeCli()
        return (cli, StoresViewModel(service: DsaService(cli: cli), text: KeyText()))
    }

    func test_fr43_each_store_offers_install_until_it_is_installed_then_open() async {
        let (cli, vm) = make()
        cli.answer("stores status --json", stores(auroraInstalled: false))
        cli.answer("stores install aurora --json", stores(auroraInstalled: true))
        await vm.load()
        XCTAssertEqual(vm.stores.map(\.name), ["ONE store", "Aurora Store", "F-Droid"])
        for store in vm.stores {
            XCTAssertEqual(vm.status(store), "AltStore_NotInstalled")
            XCTAssertTrue(vm.canInstall(store))
            XCTAssertFalse(vm.canOpen(store))
        }
        await vm.install("aurora")
        XCTAssertTrue(cli.called("stores install aurora --json"))
        let aurora = vm.stores[1]
        XCTAssertTrue(aurora.installed)
        XCTAssertEqual(aurora.version, "4.8.4")
        XCTAssertEqual(vm.status(aurora), "AltStore_InstalledVersion")
        XCTAssertFalse(vm.canInstall(aurora))
        XCTAssertTrue(vm.canOpen(aurora))
        cli.answer("launch com.aurora.store", "")
        await vm.open("aurora")
        XCTAssertTrue(cli.called("launch com.aurora.store"))
    }

    func test_fr43_a_refused_install_says_why_and_keeps_install_offered() async {
        let (cli, vm) = make()
        cli.answer("stores status --json", stores(auroraInstalled: false))
        cli.fail("stores install fdroid --json", "dsa: the file downloaded for F-Droid is signed by another certificate")
        await vm.load()
        await vm.install("fdroid")
        let fdroid = vm.stores[2]
        XCTAssertEqual(vm.status(fdroid), "AltStore_Failed")
        XCTAssertTrue(vm.canInstall(fdroid))
    }

    func test_fr43_a_service_that_cannot_list_the_stores_still_offers_all_three() async {
        let (_, vm) = make()
        await vm.load()
        XCTAssertEqual(vm.stores.map(\.id), ["onestore", "aurora", "fdroid"])
    }

    func test_fr43_alternative_store_strings_exist_in_every_language() throws {
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().appendingPathComponent("Resources/Localizable.xcstrings")
        let root = try XCTUnwrap(try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        let strings = try XCTUnwrap(root["strings"] as? [String: Any])
        let keys = ["AltStoresHeader.Text", "AltStoresCard.Description", "AltStore_NotInstalled", "AltStore_Installed",
                    "AltStore_InstalledVersion", "AltStore_Installing", "AltStore_Failed",
                    "AltStoreInstallButton.Content", "AltStoreOpenButton.Content"]
        for key in keys {
            let localizations = try XCTUnwrap((strings[key] as? [String: Any])?["localizations"] as? [String: Any], key)
            for language in InterfaceLanguage.all {
                let value = ((localizations[language] as? [String: Any])?["stringUnit"] as? [String: Any])?["value"] as? String
                XCTAssertFalse((value ?? "").isEmpty, "\(key) has no \(language) text")
            }
        }
    }
}
