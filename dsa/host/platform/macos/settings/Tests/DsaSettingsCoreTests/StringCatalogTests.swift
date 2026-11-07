import XCTest

/// The app's String Catalog: every text the app asks for is there in English and in Korean.
final class StringCatalogTests: XCTestCase {
    private var package: URL {
        URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    }

    private func catalog() throws -> [String: [String: String]] {
        let data = try Data(contentsOf: package.appendingPathComponent("Resources/Localizable.xcstrings"))
        let root = try XCTUnwrap(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(root["sourceLanguage"] as? String, "en")
        let strings = try XCTUnwrap(root["strings"] as? [String: Any])
        var texts: [String: [String: String]] = [:]
        for (key, entry) in strings {
            let localizations = (entry as? [String: Any])?["localizations"] as? [String: Any] ?? [:]
            var values: [String: String] = [:]
            for (language, unit) in localizations {
                values[language] = ((unit as? [String: Any])?["stringUnit"] as? [String: Any])?["value"] as? String
            }
            texts[key] = values
        }
        return texts
    }

    /// The literal keys the sources ask for: `get("...")`, `format("...")` and `T("...")`.
    private func keysInSources() throws -> Set<String> {
        let sources = package.appendingPathComponent("Sources")
        let files = FileManager.default.enumerator(at: sources, includingPropertiesForKeys: nil)?
            .compactMap { $0 as? URL }.filter { $0.pathExtension == "swift" } ?? []
        let pattern = try NSRegularExpression(pattern: #"\b(?:get|format|T)\("([A-Za-z0-9_.\-]+)""#)
        var keys = Set<String>()
        for file in files {
            let text = try String(contentsOf: file, encoding: .utf8)
            for match in pattern.matches(in: text, range: NSRange(text.startIndex..., in: text)) {
                if let range = Range(match.range(at: 1), in: text) {
                    keys.insert(String(text[range]))
                }
            }
        }
        return keys
    }

    func test_fr17_every_text_is_in_english_and_korean() throws {
        // U+2014, which the house style keeps out of every text.
        let emDash = String(Character(Unicode.Scalar(0x2014 as UInt32)!))
        for (key, values) in try catalog() {
            XCTAssertFalse((values["en"] ?? "").isEmpty, "\(key) has no English text")
            XCTAssertFalse((values["ko"] ?? "").isEmpty, "\(key) has no Korean text")
            XCTAssertFalse((values["en"] ?? "").contains(emDash), "\(key) has an em dash in English")
            XCTAssertFalse((values["ko"] ?? "").contains(emDash), "\(key) has an em dash in Korean")
        }
    }

    func test_fr17_every_text_the_app_asks_for_is_in_the_catalog() throws {
        let texts = try catalog()
        let used = try keysInSources()
        XCTAssertGreaterThan(used.count, 100)
        for key in used.sorted() {
            XCTAssertNotNil(texts[key], "\(key) is asked for but is not in Localizable.xcstrings")
        }
        // The keys made from a value at run time.
        var built = SessionStateKeys.all
        built += ["Role_Vm", "Role_WindowManager", "Role_AppWindow"]
        built += ["V", "D", "I", "W", "E", "F"].map { "Logs_Priority_\($0)" }
        built += ["incompatible", "conflicting-signature", "invalid-apk", "insufficient-storage", "downgrade", "blocked", "other"]
            .map { "Install_Reason_\($0)" }
        built += ["not-installed", "system-app", "blocked", "other"].map { "Apps_Refused_\($0)" }
        built += ["Documents", "Downloads", "Pictures", "Music", "Videos", "Desktop"].map { "Folder_\($0)" }
        built += ["Off", "WaitingForServer", "WaitingForAndroid", "Attached", "Detached"].map { "AdbAutoConnect_\($0)" }
        for key in ["manufacturer", "brand", "model", "device", "name", "characteristics", "fingerprint_release",
                    "fingerprint_id", "fingerprint_incremental", "fingerprint_type", "fingerprint_tags", "fingerprint"] {
            built += ["Identity_\(key)", "Identity_\(key)_Description", "Identity_\(key)_Invalid"]
        }
        for key in ["model_name", "device_name", "soc_manufacturer", "soc_model"] {
            built += ["Identity_\(key)", "Identity_\(key)_Description"]
        }
        for key in built {
            XCTAssertNotNil(texts[key], "\(key) is asked for but is not in Localizable.xcstrings")
        }
    }

    func test_fr21_the_log_page_uses_the_app_font() throws {
        let source = try String(contentsOf: package.appendingPathComponent("Sources/DsaSettings/AndroidPages.swift"), encoding: .utf8)
        let page = try XCTUnwrap(source.range(of: "struct LogsPage"))
        XCTAssertFalse(source[page.lowerBound...].contains("monospaced"), "the log page sets a monospaced font instead of the app's own")
    }

    func test_fr17_automatic_memory_limit_names_no_fixed_size() throws {
        let values = try XCTUnwrap(try catalog()["MemoryLimitCard.Description"])
        for (language, half) in [("en", "half of this Mac's memory"), ("ko", "이 Mac 메모리의 절반")] {
            let text = try XCTUnwrap(values[language])
            XCTAssertNil(
                text.range(of: #"\d\s*(GB|GiB)"#, options: [.regularExpression, .caseInsensitive]),
                "\(language) names a size, which reads as a limit on Automatic: \(text)")
            XCTAssertTrue(text.contains(half), "\(language) should say that Automatic stops at half this Mac's memory: \(text)")
        }
    }

    func test_fr17_the_mac_texts_name_the_mac_and_not_windows() throws {
        for (key, values) in try catalog() where key != "AboutIndependent.Text" {
            for language in ["en", "ko"] {
                let text = values[language] ?? ""
                XCTAssertFalse(text.contains("Windows"), "\(key) (\(language)) names Windows: \(text)")
                XCTAssertFalse(text.contains("File Explorer") || text.contains("파일 탐색기"), "\(key) (\(language)) names File Explorer")
                XCTAssertFalse(text.contains("Start menu") || text.contains("시작 메뉴"), "\(key) (\(language)) names the Start menu")
            }
        }
    }
}

private enum SessionStateKeys {
    static let all = ["Unknown", "Stopped", "Starting", "Active", "Dozing", "Frozen", "Stopping", "Stalled"].map { "State_\($0)" }
}
