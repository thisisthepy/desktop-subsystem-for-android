import XCTest
@testable import DsaSettingsCore

/// Google's ARM translator in the ARM apps section: offered only where dsa can fetch it,
/// fetched through `dsa arm-translation fetch google` once the user agrees, and a refusal that
/// leaves the section as it was.
@MainActor
final class ArmGoogleTests: XCTestCase {
    private let fetch = "arm-translation fetch google --yes --json"

    private func status(available: Bool) -> String {
        #"{"google_fetch_available":\#(available),"package":null,"enabled":true,"supported":false,"#
            + #""android":{"installed":false,"active":false,"abilist":"x86_64"},"restart_needed":false}"#
    }

    private let google = #"""
    {"google_fetch_available":true,"package":{"name":"Google ARM translator",
    "version":"BE2A.250530.026.F3 (API 36, r7)","library":"libndk_translation.so","abis":["arm64-v8a"],
    "license":"Android SDK License"},"enabled":true,"supported":true,
    "android":{"installed":true,"active":false,"abilist":"x86_64"},"restart_needed":true}
    """#

    private func make(available: Bool) -> (FakeCli, SettingsViewModel) {
        let cli = FakeCli()
        cli.answer("settings --json", "{}")
        cli.answer("arm-translation status --json", status(available: available))
        return (cli, SettingsViewModel(service: DsaService(cli: cli), text: KeyText()))
    }

    func test_fr25_googles_translator_is_fetched_and_installed_once_the_user_agrees() async {
        let (cli, settings) = make(available: true)
        cli.answer(fetch, google)
        await settings.load()
        XCTAssertTrue(settings.showsGoogleArm)
        XCTAssertTrue(settings.canFetchGoogleArm)
        XCTAssertEqual(settings.armStatus, "Arm_NotSupported")
        XCTAssertEqual(settings.googleArmTermsUrl, "https://developer.android.com/studio/terms")
        XCTAssertFalse(cli.called(fetch), "nothing is downloaded before the user agrees")

        await settings.fetchGoogleArmTranslator()
        XCTAssertTrue(cli.called(fetch))
        XCTAssertNil(settings.error)
        XCTAssertFalse(settings.isFetchingGoogleArm)
        XCTAssertTrue(settings.armSupported)
        XCTAssertEqual(settings.armStatus, "Arm_Supported")
        XCTAssertTrue(settings.armRestartNeeded, "it works from the next start")
        XCTAssertTrue(settings.canRemoveArmTranslation, "it is removed like any other package")
    }

    func test_fr25_googles_translator_is_not_offered_where_android_runs_arm_apps_itself() async {
        let (_, settings) = make(available: false)
        await settings.load()
        XCTAssertFalse(settings.showsGoogleArm)
        XCTAssertFalse(settings.canFetchGoogleArm)
    }

    func test_fr25_a_refused_download_leaves_the_section_as_it_was() async {
        let (cli, settings) = make(available: true)
        cli.fail(fetch, "dsa: the download has the SHA-1 0123, but Google's list says 4567; nothing was changed")
        await settings.load()
        await settings.fetchGoogleArmTranslator()
        XCTAssertNotNil(settings.error)
        XCTAssertFalse(settings.hasArmPackage)
        XCTAssertEqual(settings.armStatus, "Arm_NotSupported")
        XCTAssertFalse(settings.isFetchingGoogleArm)
        XCTAssertTrue(settings.canFetchGoogleArm, "it can be tried again")
    }

    func test_fr25_the_notice_names_the_source_and_googles_terms() {
        let settings = SettingsViewModel(service: DsaService(cli: FakeCli()), text: NoticeText())
        XCTAssertTrue(settings.googleArmNotice.contains("https://dl.google.com/android/repository/"))
        XCTAssertTrue(settings.googleArmNotice.contains("https://developer.android.com/studio/terms"))
    }
}

/// The notice as the app words it, with its two places for addresses.
private final class NoticeText: TextSource {
    func get(_ key: String) -> String {
        key == "ArmGoogle_Notice" ? "from {0} under {1}" : key
    }
}
