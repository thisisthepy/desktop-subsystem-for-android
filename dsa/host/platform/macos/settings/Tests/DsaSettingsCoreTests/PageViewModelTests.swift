import XCTest
@testable import DsaSettingsCore

/// The Apps, Widgets, Background, Device identity, Android, Android log pages and the APK
/// drop, through the dsa command.
@MainActor
final class PageViewModelTests: XCTestCase {
    private let app = #"{"package":"org.example.notes","label":"Notes","version_name":"2.1","system_app":false,"app_bytes":2097152}"#
    private let system = #"{"package":"com.android.settings","label":"Settings","system_app":true}"#

    func test_fr17_apps_page_finds_apps_by_name_or_package() async {
        let cli = FakeCli()
        cli.answer("apps list --json", #"{"apps":[\#(app),\#(system)]}"#)
        let apps = AppsViewModel(service: AppsService(cli: cli), text: KeyText())
        await apps.refresh()
        XCTAssertEqual(apps.rows.map(\.label), ["Notes", "Settings"])
        XCTAssertEqual(apps.count, "Apps_Count")
        apps.search = "EXAMPLE"
        XCTAssertEqual(apps.rows.map(\.package), ["org.example.notes"])
        XCTAssertEqual(apps.count, "Apps_CountMatching")
        apps.search = "nothing"
        XCTAssertTrue(apps.hasNoMatches)
        apps.search = ""
        let settings = apps.rows.first { $0.package == "com.android.settings" }
        XCTAssertEqual(settings?.canUninstall, false)
        XCTAssertEqual(settings?.uninstallTip, "Apps_SystemTip")
    }

    func test_fr17_an_uninstall_android_refuses_says_why() async {
        let cli = FakeCli()
        cli.answer("apps list --json", #"{"apps":[\#(app)]}"#)
        cli.fail("apps uninstall org.example.notes --json", "dsa: not uninstalled (blocked): DELETE_FAILED_DEVICE_POLICY_MANAGER")
        let apps = AppsViewModel(service: AppsService(cli: cli), text: KeyText())
        await apps.refresh()
        await apps.uninstall("org.example.notes")
        XCTAssertEqual(apps.error, "Apps_NotUninstalled")
        XCTAssertNil(apps.message)
    }

    /// An app with partial x86_64 code, one without native code, and one that runs as ARM with
    /// nothing to go back to, as dsa apps list --json prints them.
    private func armList(translation: Bool) -> String {
        let game = #"{"package":"com.example.game","label":"Game","system_app":false,"run_as_arm":{"on":false,"available":\#(translation),"reason":"\#(translation ? "" : "translation-off")"}}"#
        let notes = #"{"package":"com.example.notes","label":"Notes","system_app":false,"run_as_arm":{"on":false,"available":false,"reason":"no-arm-libraries"}}"#
        let tool = #"{"package":"com.example.tool","label":"Tool","system_app":false,"run_as_arm":{"on":true,"available":false,"reason":"arm-only"}}"#
        return #"{"apps":[\#(game),\#(notes),\#(tool)]}"#
    }

    func test_fr25_the_run_as_arm_switch_is_off_with_its_reason_when_it_cannot_be_changed() async {
        let cli = FakeCli()
        cli.answer("apps list --json", armList(translation: false))
        let apps = AppsViewModel(service: AppsService(cli: cli), text: KeyText())
        await apps.refresh()
        XCTAssertNil(apps.error)
        let game = apps.rows.first { $0.package == "com.example.game" }
        XCTAssertEqual(game?.canRunAsArm, false)
        XCTAssertEqual(game?.runAsArmTip, "Apps_RunAsArm_translation-off")
        let notes = apps.rows.first { $0.package == "com.example.notes" }
        XCTAssertEqual(notes?.runAsArmTip, "Apps_RunAsArm_no-arm-libraries")
        let tool = apps.rows.first { $0.package == "com.example.tool" }
        XCTAssertEqual(tool?.runAsArm, true)
        XCTAssertEqual(tool?.canRunAsArm, false)
        XCTAssertEqual(tool?.runAsArmTip, "Apps_RunAsArm_arm-only")
        // An older dsa that says nothing of the switch leaves it off and says so.
        XCTAssertEqual(apps.runAsArmTip(.unknown), "Apps_RunAsArm_unknown")
        XCTAssertNil(apps.runAsArmTip(RunAsArmSwitch(on: false, available: true, reason: "")))
    }

    func test_fr25_turning_run_as_arm_on_keeps_the_data_or_asks_before_it_goes() async {
        let cli = FakeCli()
        cli.answer("apps list --json", armList(translation: true))
        cli.fail("apps run-as-arm com.example.game on --json",
                 "dsa: not changed (data-would-be-erased): com.example.game: Android cannot change how the app runs and keep its data")
        cli.answer("apps run-as-arm com.example.game on --erase-data --json",
                   #"{"package":"com.example.game","status":"done","on":true,"message":""}"#)
        let apps = AppsViewModel(service: AppsService(cli: cli), text: KeyText())
        await apps.refresh()
        XCTAssertEqual(apps.rows.first { $0.package == "com.example.game" }?.canRunAsArm, true)

        // The user keeps the data: nothing is erased.
        var asked: [String] = []
        await apps.setRunAsArm("com.example.game", true) { label in
            asked.append(label)
            return false
        }
        XCTAssertEqual(asked, ["Game"])
        XCTAssertEqual(apps.message, "Apps_RunAsArmKept")
        XCTAssertFalse(cli.called("apps run-as-arm com.example.game on --erase-data --json"))

        // The user lets it go: the app is installed again and its data erased.
        await apps.setRunAsArm("com.example.game", true) { _ in true }
        XCTAssertNil(apps.error)
        XCTAssertTrue(cli.called("apps run-as-arm com.example.game on --erase-data --json"))
        XCTAssertEqual(apps.message, "Apps_RunAsArmOn")
    }

    func test_fr17_app_window_settings_go_through_app_window() async {
        let cli = FakeCli()
        cli.answer("apps list --json", #"{"apps":[\#(app)]}"#)
        cli.answer("app-window --json", #"{"apps":[]}"#)
        cli.answer("app-window org.example.notes --fixed-size off --start-size 412x915 --json",
                   #"{"apps":[{"package":"org.example.notes","fixed_size":false,"start_size":[412,915]}]}"#)
        let apps = AppsViewModel(service: AppsService(cli: cli), text: KeyText())
        await apps.refresh()
        await apps.loadWindowSettings()
        XCTAssertEqual(apps.rows.first?.startSizeIndex, 0)
        await apps.setStartSize("org.example.notes", 1)
        XCTAssertEqual(apps.rows.first?.startSizeIndex, 1)
        XCTAssertEqual(apps.message, "Apps_WindowChanged")
    }

    func test_fr17_widgets_placed_elsewhere_show_on_the_page() async {
        let cli = FakeCli()
        cli.answer("widgets list --json", #"{"providers":[{"provider":"org.example/.Clock","package":"org.example","label":"Clock","default_width_dp":110,"default_height_dp":40,"needs_configuration":true}],"widgets":[]}"#)
        cli.streams["widgets watch --json"] = [
            #"{"event":"placed","widget":{"id":3,"provider":"org.example/.Clock","label":"Clock","placement":{"x":10,"y":20,"width_dp":110,"height_dp":40}}}"#,
            "not json",
            #"{"event":"placed","widget":{"id":1,"provider":"org.example/.Clock","label":"Clock"}}"#,
            #"{"event":"removed","id":3}"#,
        ]
        let widgets = WidgetsViewModel(service: DsaService(cli: cli), text: KeyText(), stream: cli)
        await widgets.refresh()
        XCTAssertTrue(widgets.hasNoPlaced)
        XCTAssertEqual(widgets.offers.first?.app, "org.example")
        XCTAssertEqual(widgets.offers.first?.detail, "Widgets_Size · Widgets_Setup")
        try? await widgets.watchOnce()
        XCTAssertEqual(widgets.placed.map(\.id), [1])
        XCTAssertEqual(widgets.offers.count, 1)
    }

    func test_fr17_background_apps_show_what_they_play() async {
        let cli = FakeCli()
        cli.answer("settings --json", #"{"stop_without_asking":true}"#)
        cli.answer("background list --json", #"{"apps":[{"package":"org.example.music","label":"Music","title":"Playing","text":""}],"media":[{"package":"org.example.music","title":"Song","artist":"Band","playback":"playing"}]}"#)
        let background = BackgroundViewModel(service: DsaService(cli: cli), text: KeyText())
        await background.refresh()
        XCTAssertFalse(background.askBeforeStop)
        XCTAssertEqual(background.apps.first?.detail, "Playing\nBackground_Playing")
        cli.answer("background stop org.example.music --json", "{}")
        await background.stop("org.example.music")
        XCTAssertEqual(background.message, "Background_Stopped")
    }

    func test_fr17_device_identity_checks_values_before_sending_them() {
        XCTAssertNil(DeviceIdentityRules.check("model", ""))
        XCTAssertNil(DeviceIdentityRules.check("model", "Pixel 9"))
        XCTAssertEqual(DeviceIdentityRules.check("model", " Pixel"), "Identity_model_Invalid")
        XCTAssertEqual(DeviceIdentityRules.check("brand", "-x"), "Identity_brand_Invalid")
        XCTAssertNil(DeviceIdentityRules.check("characteristics", "tablet,nosdcard"))
        XCTAssertEqual(DeviceIdentityRules.check("fingerprint.type", "debug"), "Identity_fingerprint_type_Invalid")
        XCTAssertTrue(DeviceIdentityRules.isFingerprint("google/panther/panther:14/UP1A/123:user/release-keys"))
        XCTAssertFalse(DeviceIdentityRules.isFingerprint("google/panther:14/UP1A/123:user/release-keys"))
    }

    func test_fr17_device_identity_sends_only_what_changed() async {
        let cli = FakeCli()
        let report = #"{"chosen":{},"effective":{"brand":"dsa","name":"dsa_x86_64","device":"dsa","fingerprint_release":"15","fingerprint_id":"AP3A","fingerprint_incremental":"1","fingerprint_type":"user","fingerprint_tags":"release-keys","fingerprint":"dsa/dsa_x86_64/dsa:15/AP3A/1:user/release-keys"}}"#
        cli.answer("settings device --json", report)
        cli.answer("status --json", #"{"state":"Active"}"#)
        cli.answer("settings set device.brand acme --json", #"{"chosen":{"brand":"acme"},"effective":{"brand":"acme"}}"#)
        let identity = DeviceIdentityViewModel(service: DsaService(cli: cli), text: KeyText())
        await identity.load()
        XCTAssertFalse(identity.canSave)
        identity.edit("brand", "acme")
        XCTAssertEqual(identity.fingerprintPreview, "acme/dsa_x86_64/dsa:15/AP3A/1:user/release-keys")
        identity.edit("model", " bad")
        XCTAssertFalse(identity.canSave)
        XCTAssertEqual(identity.problem(identity.field("model")), "Identity_model_Invalid")
        identity.edit("model", "")
        XCTAssertTrue(identity.canSave)
        await identity.save()
        let changes = cli.calls.filter { $0.starts(with: ["settings", "set"]) || $0.starts(with: ["settings", "reset"]) }
        XCTAssertEqual(changes.map { $0.joined(separator: " ") },
                       ["settings set device.brand acme --json"])
        XCTAssertEqual(identity.message, "Identity_SavedRestart")
        XCTAssertTrue(identity.needsRestart)
    }

    func test_fr17_device_identity_shows_the_macs_values_and_where_each_default_came_from() async {
        let cli = FakeCli()
        let report = #"{"chosen":{"model":""},"effective":{"manufacturer":"Apple","model":"Mac15,3","device":"dsa_arm64","model_name":"MacBook Pro (14-inch, M3, Nov 2023)","device_name":"BREW's MacBook Pro","soc_manufacturer":"Apple","soc_model":""},"sources":{"manufacturer":"computer","model":"computer","device":"dsa","model_name":"computer","device_name":"computer","soc_manufacturer":"computer","soc_model":"dsa"}}"#
        cli.answer("settings device --json", report)
        cli.answer("status --json", #"{"state":"Stopped"}"#)
        cli.answer("settings reset device --json", report)
        let identity = DeviceIdentityViewModel(service: DsaService(cli: cli), text: KeyText())
        await identity.load()
        XCTAssertEqual(identity.field("model").placeholder, "Mac15,3")
        XCTAssertEqual(identity.field("model").origin, "computer")
        XCTAssertEqual(identity.originText(identity.field("model").origin), "Identity_FromComputer")
        XCTAssertEqual(identity.field("device").origin, "dsa")
        XCTAssertEqual(identity.field("fingerprint.type").origin, "dsa")
        XCTAssertEqual(identity.computerValues.map(\.key), ["model_name", "device_name", "soc_manufacturer", "soc_model"])
        XCTAssertEqual(identity.computerValue("model_name").value, "MacBook Pro (14-inch, M3, Nov 2023)")
        XCTAssertEqual(identity.computerValue("device_name").value, "BREW's MacBook Pro")
        XCTAssertEqual(identity.shown(identity.computerValue("soc_model")), "Identity_NotSaid")
        XCTAssertEqual(identity.computerValue("soc_model").origin, "dsa")
        await identity.reset()
        XCTAssertEqual(identity.field("model").value, "")
        XCTAssertEqual(identity.field("model").placeholder, "Mac15,3")
    }

    func test_fr17_moving_to_an_older_android_needs_the_data_erased() async {
        let cli = FakeCli()
        cli.answer("android version --json", #"{"version":"16.0.1","android_major":16,"channel":"latest","supported_majors":[15,16],"target_major":16}"#)
        let android = AndroidVersionViewModel(cli: cli, text: KeyText())
        await android.load()
        XCTAssertEqual(android.choices.map(\.channel), ["latest", "16", "15"])
        XCTAssertEqual(android.selectedChannel, "latest")
        XCTAssertFalse(android.canApply)
        android.selectedChannel = "15"
        XCTAssertTrue(android.canApply)
        XCTAssertEqual(android.selectedMove, .down)
        XCTAssertEqual(AndroidVersionViewModel.useArguments(android.selected!, eraseData: true),
                       ["android", "use", "15", "--erase-data", "--json"])
        XCTAssertFalse(android.isSwitching)
    }

    func test_fr17_log_page_filters_and_pauses() {
        let cli = FakeCli()
        let logs = LogViewModel(stream: cli, service: DsaService(cli: cli), text: KeyText())
        logs.addMany([
            #"{"time":"01-01 00:00:00.000","priority":"I","tag":"ActivityManager","pid":12,"tid":34,"message":"Start proc"}"#,
            #"{"time":"01-01 00:00:00.001","priority":"W","tag":"Zygote","pid":1,"tid":1,"message":"slow"}"#,
            "garbage",
        ])
        XCTAssertEqual(logs.lines.count, 2)
        XCTAssertEqual(logs.lines.first?.text, "01-01 00:00:00.000    12    34 I ActivityManager: Start proc")
        logs.filter = "zygote"
        XCTAssertEqual(logs.lines.map(\.tag), ["Zygote"])
        logs.paused = true
        logs.add(#"{"tag":"Zygote","message":"again"}"#)
        XCTAssertEqual(logs.waiting, 1)
        XCTAssertEqual(logs.lines.count, 1)
        logs.paused = false
        XCTAssertEqual(logs.lines.count, 2)
        XCTAssertEqual(LogViewModel.arguments("W"), ["logcat", "--json", "*:W"])
    }

    func test_fr21_a_log_line_splits_into_the_page_columns_and_a_dropped_record_has_only_its_body() throws {
        let line = try XCTUnwrap(LogLine.parse(
            #"{"time":"2026-11-04 10:00:00.000","priority":"I","tag":"ActivityManager","pid":12,"tid":13,"message":"시작"}"#))
        XCTAssertEqual(line.timeColumn, "2026-11-04 10:00:00.000")
        XCTAssertEqual(line.pidColumn, "12")
        XCTAssertEqual(line.tidColumn, "13")
        XCTAssertEqual(line.priorityColumn, "I")
        XCTAssertEqual(line.bodyColumn, "ActivityManager: 시작")

        let dropped = try XCTUnwrap(LogLine.parse(#"{"time":"2026-11-04 10:00:00.000","priority":"W","dropped":3}"#))
        XCTAssertEqual(dropped.timeColumn + dropped.pidColumn + dropped.tidColumn + dropped.priorityColumn, "")
        XCTAssertEqual(dropped.bodyColumn, "--------- 3 records dropped")
    }

    func test_fr21_copying_selected_log_lines_gives_each_whole_line_in_page_order() {
        let cli = FakeCli()
        let logs = LogViewModel(stream: cli, service: DsaService(cli: cli), text: KeyText())
        let record = { (tag: String, message: String) in
            #"{"time":"2026-11-04 10:00:00.000","priority":"I","tag":"\#(tag)","pid":12,"tid":13,"message":"\#(message)"}"#
        }
        logs.addMany([record("A", "one"), record("B", "two"), record("C", "three"), #"{"dropped":3}"#])
        let selected: Set<LogLine.ID> = [logs.lines[3].id, logs.lines[0].id, logs.lines[2].id]
        XCTAssertEqual(logs.copyText(selected), [
            "2026-11-04 10:00:00.000    12    13 I A: one",
            "2026-11-04 10:00:00.000    12    13 I C: three",
            "--------- 3 records dropped",
        ].joined(separator: "\n"))
        XCTAssertEqual(logs.copyText([]), "")
    }

    func test_fr17_log_page_reads_the_log_through_dsa_logcat() async throws {
        let cli = FakeCli()
        cli.streams["logcat --json *:I"] = [#"{"tag":"A","message":"one"}"#, #"{"tag":"B","message":"two"}"#]
        let logs = LogViewModel(stream: cli, service: DsaService(cli: cli), text: KeyText())
        logs.start()
        for _ in 0..<50 where logs.isReading {
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        XCTAssertEqual(logs.lines.map(\.tag), ["A", "B"])
        XCTAssertFalse(logs.isReading)
    }

    func test_fr17_a_dropped_apk_is_installed_and_others_are_named() async {
        let cli = FakeCli()
        cli.answer("install /tmp/notes.apk --json", #"{"installed":true,"status":"installed","package":"org.example.notes"}"#)
        cli.fail("install /tmp/old.apk --json", "dsa: not installed (downgrade): INSTALL_FAILED_VERSION_DOWNGRADE")
        let install = InstallViewModel(service: DsaService(cli: cli), text: KeyText())
        XCTAssertTrue(install.accepts(["/tmp/notes.APK"]))
        XCTAssertFalse(install.accepts(["/tmp/notes.txt"]))
        // Split-APK bundles are installed as APKs are, and App Bundles told why they are not.
        for bundle in ["/tmp/notes.xapk", "/tmp/notes.apks", "/tmp/notes.APKM", "/tmp/notes.aab"] {
            XCTAssertTrue(install.accepts([bundle]), bundle)
        }
        await install.drop(["/tmp/notes.apk", "/tmp/old.apk", "/tmp/readme.txt"])
        XCTAssertEqual(install.severity, .error)
        XCTAssertEqual(install.title, "Install_FailedTitle")
        XCTAssertEqual(install.message, "Install_Installed\nInstall_Refused\nInstall_NotApk")
        XCTAssertTrue(install.isOpen)
    }

    func test_fr17_dsa_is_found_beside_the_app_first() {
        let found = ProcessDsaCli.locate(environment: [:], programFolder: URL(fileURLWithPath: "/Applications/DSA.app/Contents/MacOS")) {
            $0 == "/Applications/DSA.app/Contents/MacOS/dsa"
        }
        XCTAssertEqual(found, "/Applications/DSA.app/Contents/MacOS/dsa")
        XCTAssertEqual(ProcessDsaCli.locate(environment: ["DSA_CLI": "/x/dsa"], programFolder: nil) { _ in false }, "/x/dsa")
    }
}
