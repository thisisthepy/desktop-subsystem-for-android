import XCTest
@testable import DsaSettingsCore

/// The Network page's trusted certificates: the switch, on by default, that lets Android trust
/// the root certificates a user or an administrator added to this Mac, and the list of the ones
/// the service found.
@MainActor
final class HostCertificatesTests: XCTestCase {
    private let found = #"""
    {"host_certificates":true,"imported_certificates":[
     {"subject":"CN=Example Root, O=Example","issuer":"CN=Example Root, O=Example","source":"admin",
      "location":"/Library/Keychains/System.keychain","sha256":"0a1b2c","not_after":1893456000},
     {"subject":"CN=Filter CA","issuer":"CN=Filter Root","source":"user",
      "location":"login.keychain-db","sha256":"3d4e5f","not_after":0},
     {"subject":"CN=Store CA","issuer":"CN=Store CA","source":"some_new_store","location":"","sha256":"6a7b8c"}
    ]}
    """#

    private func make() -> (FakeCli, DsaService) {
        let cli = FakeCli()
        return (cli, DsaService(cli: cli))
    }

    func test_fr28_host_certificates_switch_sends_the_setting() async {
        let (cli, service) = make()
        cli.answer("settings --json", found)
        cli.answer("settings set host-certificates off --json",
                   found.replacingOccurrences(of: #""host_certificates":true"#, with: #""host_certificates":false"#))
        cli.answer("settings set host-certificates on --json", found)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertTrue(settings.hostCertificates)

        await settings.setHostCertificates(false)
        XCTAssertFalse(settings.hostCertificates)
        XCTAssertNil(settings.error)
        // The list stays: the service lists what it found whether or not Android trusts it.
        XCTAssertEqual(settings.importedCertificates.count, 3)
        await settings.setHostCertificates(true)
        XCTAssertTrue(settings.hostCertificates)
        XCTAssertTrue(cli.calls.contains(["settings", "set", "host-certificates", "off", "--json"]))
        XCTAssertTrue(cli.calls.contains(["settings", "set", "host-certificates", "on", "--json"]))
    }

    func test_fr28_setting_the_switch_to_what_it_is_sends_nothing() async {
        let (cli, service) = make()
        cli.answer("settings --json", found)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        await settings.setHostCertificates(true)
        XCTAssertFalse(cli.calls.contains { $0.starts(with: ["settings", "set"]) })
    }

    func test_fr28_a_refused_change_leaves_the_switch_as_the_service_keeps_it() async {
        let (cli, service) = make()
        cli.answer("settings --json", found)
        cli.fail("settings set host-certificates off --json", "dsa: the service refused the change")
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        await settings.setHostCertificates(false)
        XCTAssertTrue(settings.hostCertificates)
        XCTAssertNotNil(settings.error)
    }

    func test_fr28_imported_certificates_are_listed_with_their_source() async {
        let (cli, service) = make()
        cli.answer("settings --json", found)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertFalse(settings.hasNoImportedCertificates)
        XCTAssertEqual(settings.importedCertificates, [
            ImportedCertificate(subject: "CN=Example Root, O=Example", issuer: "CN=Example Root, O=Example", source: "admin",
                                location: "/Library/Keychains/System.keychain", sha256: "0a1b2c", notAfter: 1893456000),
            ImportedCertificate(subject: "CN=Filter CA", issuer: "CN=Filter Root", source: "user",
                                location: "login.keychain-db", sha256: "3d4e5f", notAfter: 0),
            ImportedCertificate(subject: "CN=Store CA", issuer: "CN=Store CA", source: "some_new_store",
                                location: "", sha256: "6a7b8c", notAfter: 0),
        ])
        XCTAssertEqual(settings.importedCertificates.map { settings.certificateSourceText($0.source) },
                       ["HostCertificatesSource_admin", "HostCertificatesSource_user", "some_new_store"])
        XCTAssertEqual(settings.certificateIssuerText(settings.importedCertificates[1]), "HostCertificatesIssuer.Text")
    }

    func test_fr28_every_known_source_has_a_name_of_its_own() {
        let settings = SettingsViewModel(service: DsaService(cli: FakeCli()), text: KeyText())
        for source in ["machine", "machine_group_policy", "machine_enterprise", "user", "user_group_policy", "admin", "system"] {
            XCTAssertEqual(settings.certificateSourceText(source), "HostCertificatesSource_\(source)")
        }
    }

    func test_fr28_the_issuer_line_names_the_issuer() async {
        final class IssuerText: TextSource {
            func get(_ key: String) -> String { key == "HostCertificatesIssuer.Text" ? "Issued by {0}" : key }
        }
        let (cli, service) = make()
        cli.answer("settings --json", found)
        let settings = SettingsViewModel(service: service, text: IssuerText())
        await settings.load()
        XCTAssertEqual(settings.certificateIssuerText(settings.importedCertificates[1]), "Issued by CN=Filter Root")
    }

    func test_fr28_no_certificate_found_reads_as_an_empty_list() async {
        let (cli, service) = make()
        cli.answer("settings --json", #"{"host_certificates":true,"imported_certificates":[]}"#)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertTrue(settings.importedCertificates.isEmpty)
        XCTAssertTrue(settings.hasNoImportedCertificates)
    }

    func test_fr28_missing_certificate_keys_read_as_on_and_empty() async {
        let (cli, service) = make()
        cli.answer("settings --json", #"{"advanced_networking":false}"#)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertTrue(settings.hostCertificates)
        XCTAssertTrue(settings.importedCertificates.isEmpty)
        XCTAssertTrue(settings.hasNoImportedCertificates)
    }

    func test_fr28_before_the_settings_are_read_the_switch_reads_as_on() {
        let settings = SettingsViewModel(service: DsaService(cli: FakeCli()), text: KeyText())
        XCTAssertTrue(settings.hostCertificates)
        XCTAssertTrue(settings.importedCertificates.isEmpty)
    }
}
