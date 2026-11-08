import XCTest
@testable import DsaSettingsCore

/// A task window's App Info opens the Apps page at the app through a link.
final class AppPageLinkTests: XCTestCase {
    func test_fr22_app_info_opens_the_apps_page_at_the_app() {
        XCTAssertEqual(AppPageLink.package(of: URL(string: "dsa-settings://apps/com.example.notes")!), "com.example.notes")
        XCTAssertEqual(AppPageLink.package(of: URL(string: "DSA-SETTINGS://Apps/com.example.notes/")!), "com.example.notes")
    }

    func test_fr22_a_link_that_names_no_app_opens_nothing() {
        XCTAssertNil(AppPageLink.package(of: URL(string: "dsa-settings://apps/notes")!))
        XCTAssertNil(AppPageLink.package(of: URL(string: "dsa-settings://apps/..%2Fevil")!))
        XCTAssertNil(AppPageLink.package(of: URL(string: "dsa-settings://memory/com.example.notes")!))
        XCTAssertNil(AppPageLink.package(of: URL(string: "https://apps/com.example.notes")!))
        XCTAssertNil(AppPageLink.package(of: URL(string: "dsa-settings://apps/")!))
    }

    func test_fr15_a_click_on_update_news_opens_the_updates_page() {
        XCTAssertTrue(AppPageLink.isUpdates(URL(string: "dsa-settings://updates")!))
        XCTAssertTrue(AppPageLink.isUpdates(URL(string: "DSA-SETTINGS://Updates/")!))
        XCTAssertFalse(AppPageLink.isUpdates(URL(string: "dsa-settings://apps/com.example.notes")!))
        XCTAssertFalse(AppPageLink.isUpdates(URL(string: "dsa-settings://updates/evil")!))
        XCTAssertFalse(AppPageLink.isUpdates(URL(string: "https://updates")!))
        XCTAssertNil(AppPageLink.package(of: URL(string: "dsa-settings://updates")!))
    }
}
