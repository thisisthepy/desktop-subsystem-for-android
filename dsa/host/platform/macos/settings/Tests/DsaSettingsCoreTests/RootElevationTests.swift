import XCTest
@testable import DsaSettingsCore

/// Root access: the Mac's switch on the Advanced page, which only an administrator changes,
/// and each app's level on the Apps page, through the dsa command.
@MainActor
final class RootElevationTests: XCTestCase {
    private let off = #"{"render_gpu":"display","log_dir":"/Library/Application Support/DSA/logs","root_elevation":false}"#
    private let on = #"{"render_gpu":"display","log_dir":"/Library/Application Support/DSA/logs","root_elevation":true}"#

    /// A backup app at "ask", a notes app at "off" and Settings at "remember", as dsa apps list
    /// --json prints them while root elevation is on.
    private let apps = #"{"apps":[{"package":"com.example.backup","label":"Backup","root":"ask"},{"package":"com.example.notes","label":"Notes","root":"off"},{"package":"com.android.settings","label":"Settings","system_app":true,"root":"remember"}]}"#

    func test_fr35_root_elevation_is_off_by_default_and_only_an_administrator_changes_it() async {
        let cli = FakeCli()
        cli.answer("settings --json", #"{"render_gpu":"display"}"#)
        let settings = SettingsViewModel(service: DsaService(cli: cli), text: KeyText())
        await settings.load()
        XCTAssertFalse(settings.rootElevation)
        XCTAssertFalse(settings.canChangeRootElevation)
        XCTAssertTrue(settings.rootElevationNeedsAdministrator)
        settings.isAdministrator = true
        XCTAssertTrue(settings.canChangeRootElevation)
        XCTAssertFalse(settings.rootElevationNeedsAdministrator)
    }

    func test_fr35_an_administrator_turns_root_elevation_on_and_the_page_says_it_applies_from_the_next_start() async {
        let cli = FakeCli()
        cli.answer("settings --json", off)
        cli.answer("settings set root-elevation on --json", on)
        let settings = SettingsViewModel(service: DsaService(cli: cli), text: KeyText())
        settings.isAdministrator = true
        await settings.load()
        XCTAssertFalse(settings.rootElevationRestartNeeded)
        await settings.setRootElevation(true)
        XCTAssertNil(settings.error)
        XCTAssertTrue(settings.rootElevation)
        XCTAssertTrue(cli.called("settings set root-elevation on --json"))
        XCTAssertTrue(settings.rootElevationRestartNeeded)

        // Setting it to what it is sends nothing.
        let calls = cli.calls.count
        await settings.setRootElevation(true)
        XCTAssertEqual(cli.calls.count, calls)
    }

    func test_fr35_a_refused_change_leaves_the_switch_as_the_service_keeps_it() async {
        let cli = FakeCli()
        cli.answer("settings --json", off)
        cli.fail("settings set root-elevation on --json", "dsa: only an administrator may change root elevation")
        let settings = SettingsViewModel(service: DsaService(cli: cli), text: KeyText())
        settings.isAdministrator = true
        await settings.load()
        await settings.setRootElevation(true)
        XCTAssertNotNil(settings.error)
        XCTAssertFalse(settings.rootElevation)
        XCTAssertFalse(settings.rootElevationRestartNeeded)
    }

    func test_fr35_the_page_says_where_the_requests_are_recorded() async {
        let cli = FakeCli()
        cli.answer("settings --json", off)
        let settings = SettingsViewModel(service: DsaService(cli: cli), text: KeyText())
        await settings.load()
        XCTAssertEqual(settings.rootElevationLog, "RootElevation_LogAt")

        let older = FakeCli()
        older.answer("settings --json", #"{"render_gpu":"display"}"#)
        let without = SettingsViewModel(service: DsaService(cli: older), text: KeyText())
        await without.load()
        XCTAssertEqual(without.rootElevationLog, "RootElevation_LogUnknown")
    }

    func test_fr35_each_app_shows_its_root_level_off_ask_or_remember() async {
        let cli = FakeCli()
        cli.answer("apps list --json", apps)
        let vm = AppsViewModel(service: AppsService(cli: cli), text: KeyText())
        await vm.refresh()
        XCTAssertNil(vm.error)
        XCTAssertFalse(vm.rootUnavailable)
        XCTAssertEqual(vm.rootChoices.map(\.level), [.off, .ask, .remember])
        XCTAssertEqual(vm.rootChoices.map(\.label), ["Apps_Root_off", "Apps_Root_ask", "Apps_Root_remember"])
        let backup = vm.rows.first { $0.package == "com.example.backup" }
        XCTAssertEqual(backup?.rootIndex, RootLevel.ask.index)
        XCTAssertEqual(backup?.canChangeRoot, true)
        XCTAssertNil(backup?.rootTip ?? nil)
        XCTAssertEqual(vm.rows.first { $0.package == "com.example.notes" }?.rootIndex, RootLevel.off.index)
        XCTAssertEqual(vm.rows.first { $0.package == "com.android.settings" }?.rootIndex, RootLevel.remember.index)
    }

    func test_fr35_apps_sharing_a_user_id_are_approved_as_one_group() async {
        let cli = FakeCli()
        cli.answer("apps list --json", #"{"apps":[{"package":"com.example.zeta","label":"Zeta","root":"ask","shared_user_id":"com.example.shared"},{"package":"com.example.middle","label":"Middle","root":"off"},{"package":"com.example.alpha","label":"Alpha","root":"ask","shared_user_id":"com.example.shared"}]}"#)
        let vm = AppsViewModel(service: AppsService(cli: cli), text: KeyText())
        await vm.refresh()
        XCTAssertNil(vm.error)
        // The group is listed together, in the place of its first app by name.
        XCTAssertEqual(vm.rows.map(\.label), ["Alpha", "Zeta", "Middle"])
        // Each app of the group says its root level is one setting with the others.
        XCTAssertEqual(vm.rows.first { $0.package == "com.example.zeta" }?.rootGroupNote, "Apps_RootGroup")
        XCTAssertNil(vm.rows.first { $0.package == "com.example.middle" }?.rootGroupNote ?? nil)
        // A search that finds one app of the group shows the whole group.
        vm.search = "zeta"
        XCTAssertEqual(vm.rows.map(\.label), ["Alpha", "Zeta"])
    }

    func test_fr35_no_app_level_can_be_chosen_while_root_elevation_is_off_for_the_mac() async {
        let cli = FakeCli()
        cli.answer("apps list --json", #"{"apps":[{"package":"com.example.backup","label":"Backup","root":"unavailable"}]}"#)
        let vm = AppsViewModel(service: AppsService(cli: cli), text: KeyText())
        await vm.refresh()
        XCTAssertTrue(vm.rootUnavailable)
        let row = vm.rows.first
        XCTAssertEqual(row?.canChangeRoot, false)
        XCTAssertEqual(row?.rootIndex, -1)
        XCTAssertEqual(row?.rootTip, "Apps_RootTip_unavailable")

        // A dsa from before root levels says nothing; the list cannot be used either.
        let older = FakeCli()
        older.answer("apps list --json", #"{"apps":[{"package":"com.example.notes","label":"Notes"}]}"#)
        let old = AppsViewModel(service: AppsService(cli: older), text: KeyText())
        await old.refresh()
        XCTAssertFalse(old.rootUnavailable)
        XCTAssertEqual(old.rows.first?.canChangeRoot, false)
        XCTAssertEqual(old.rows.first?.rootTip, "Apps_RootTip_unknown")
    }

    func test_fr35_choosing_a_level_goes_through_dsa_apps_root() async {
        let cli = FakeCli()
        cli.answer("apps list --json", apps)
        cli.answer("apps root com.example.notes ask --json", #"{"package":"com.example.notes","level":"ask"}"#)
        let vm = AppsViewModel(service: AppsService(cli: cli), text: KeyText())
        await vm.refresh()
        cli.answer("apps list --json", apps.replacingOccurrences(of: #""label":"Notes","root":"off""#, with: #""label":"Notes","root":"ask""#))
        await vm.setRoot("com.example.notes", RootLevel.ask.index)
        XCTAssertNil(vm.error)
        XCTAssertTrue(cli.called("apps root com.example.notes ask --json"))
        XCTAssertEqual(vm.message, "Apps_RootSet_ask")
        XCTAssertEqual(vm.rows.first { $0.package == "com.example.notes" }?.rootIndex, RootLevel.ask.index)

        // Choosing the level the app has sends nothing.
        let calls = cli.calls.count
        await vm.setRoot("com.example.notes", RootLevel.ask.index)
        XCTAssertEqual(cli.calls.count, calls)
    }

    func test_fr35_remember_without_an_administrators_approval_is_refused_with_the_services_words() async {
        let cli = FakeCli()
        cli.answer("apps list --json", apps)
        cli.fail("apps root com.example.backup remember --json",
                 "dsa: not changed: com.example.backup: letting an app keep root without asking needs an administrator")
        let vm = AppsViewModel(service: AppsService(cli: cli), text: RootText())
        await vm.refresh()
        await vm.setRoot("com.example.backup", RootLevel.remember.index)
        XCTAssertEqual(vm.error, "Backup kept: letting an app keep root without asking needs an administrator")
        XCTAssertNil(vm.message)
        XCTAssertEqual(vm.rows.first { $0.package == "com.example.backup" }?.rootIndex, RootLevel.ask.index)
    }
}

/// Gives each text's key back, except the refusal, whose words the test reads.
private final class RootText: TextSource {
    func get(_ key: String) -> String { key == "Apps_RootNotChanged" ? "{0} kept: {1}" : key }
}
