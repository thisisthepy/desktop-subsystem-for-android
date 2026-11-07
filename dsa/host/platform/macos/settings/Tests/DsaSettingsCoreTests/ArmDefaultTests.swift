import XCTest
@testable import DsaSettingsCore

/// The default ARM translator in the ARM apps section: the one an x86_64 image carries is used
/// from the first start, a package the user installs replaces it, and the section goes back to
/// it through `dsa arm-translation use-default`. An image without one (every arm64 image, which
/// an Apple silicon Mac runs) shows nothing of it.
@MainActor
final class ArmDefaultTests: XCTestCase {
    private let useDefault = "arm-translation use-default --json"

    private let digitalis = #"{"name":"Digitalis","version":"17-abefbe2b1600","library":"libberberis_arm64.so","abis":["arm64-v8a"],"license":"Apache-2.0"}"#
    private let mine = #"{"name":"Translator","version":"1.0","library":"libtranslator.so","abis":["arm64-v8a"],"license":"Apache-2.0"}"#

    /// What `dsa arm-translation --json` says, with `source` naming where the translator in use
    /// comes from ("" for none).
    private func status(package: String, builtIn: String, source: String, enabled: Bool = true) -> String {
        let used = source == "default" ? builtIn : package
        let translator = source.isEmpty ? "null" : #"{"source":"\#(source)","# + String(used.dropFirst())
        return #"{"google_fetch_available":false,"package":\#(package),"default":\#(builtIn),"#
            + #""translator":\#(translator),"enabled":\#(enabled),"supported":\#(!source.isEmpty),"#
            + #""android":{"installed":\#(package != "null"),"active":true,"abilist":"x86_64,arm64-v8a"},"#
            + #""restart_needed":false}"#
    }

    private func make(_ answer: String) -> (FakeCli, SettingsViewModel) {
        let cli = FakeCli()
        cli.answer("settings --json", "{}")
        cli.answer("arm-translation status --json", answer)
        return (cli, SettingsViewModel(service: DsaService(cli: cli), text: FormatText()))
    }

    func test_fr25_the_default_translator_is_used_from_the_first_start() async {
        let (_, settings) = make(status(package: "null", builtIn: digitalis, source: "default"))
        await settings.load()
        XCTAssertTrue(settings.armSupported)
        XCTAssertEqual(settings.armStatus, "default Digitalis 17-abefbe2b1600 Apache-2.0")
        XCTAssertTrue(settings.hasArmDefault)
        XCTAssertFalse(settings.showsArmRemove, "removing a package is going back to the default")
        XCTAssertEqual(settings.armDefaultDescription, "about Digitalis 17-abefbe2b1600 Apache-2.0")
        XCTAssertFalse(settings.canUseDefaultArmTranslator, "already in use")
    }

    func test_fr25_a_package_replaces_the_default_until_the_user_goes_back() async {
        let (cli, settings) = make(status(package: mine, builtIn: digitalis, source: "user"))
        cli.answer(useDefault, status(package: "null", builtIn: digitalis, source: "default"))
        await settings.load()
        XCTAssertEqual(settings.armStatus, "user Translator 1.0 Apache-2.0")
        XCTAssertTrue(settings.canUseDefaultArmTranslator)
        XCTAssertFalse(cli.called(useDefault))

        await settings.useDefaultArmTranslator()
        XCTAssertTrue(cli.called(useDefault))
        XCTAssertNil(settings.error)
        XCTAssertFalse(settings.hasArmPackage)
        XCTAssertEqual(settings.armStatus, "default Digitalis 17-abefbe2b1600 Apache-2.0")
        XCTAssertFalse(settings.canUseDefaultArmTranslator)
    }

    func test_fr25_turned_off_the_section_names_the_default_translator_it_does_not_use() async {
        let (_, settings) = make(status(package: "null", builtIn: digitalis, source: "", enabled: false))
        await settings.load()
        XCTAssertFalse(settings.armSupported)
        XCTAssertEqual(settings.armStatus, "off Digitalis 17-abefbe2b1600")
    }

    func test_fr25_an_image_without_a_default_translator_shows_nothing_of_it() async {
        let (_, settings) = make(status(package: "null", builtIn: "null", source: ""))
        await settings.load()
        XCTAssertFalse(settings.hasArmDefault)
        XCTAssertTrue(settings.showsArmRemove)
        XCTAssertFalse(settings.canUseDefaultArmTranslator)
        XCTAssertEqual(settings.armDefaultDescription, "")
        XCTAssertEqual(settings.armStatus, "Arm_NotSupported")
    }
}

/// The section's texts with their places filled, so a test sees which one shows and with what.
private final class FormatText: TextSource {
    func get(_ key: String) -> String {
        switch key {
        case "Arm_SupportedDefault": return "default {0} {1} {2}"
        case "Arm_Supported": return "user {0} {1} {2}"
        case "Arm_DefaultTurnedOff": return "off {0} {1}"
        case "Arm_DefaultDescription": return "about {0} {1} {2}"
        default: return key
        }
    }
}
