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
        built += ["Role_Vm", "Role_WindowManager", "Role_Renderer", "Role_AppWindow"]
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

    /// The width a paragraph takes, in columns: a Hangul syllable or another East Asian wide
    /// character counts 2, every other character 1. Placeholders count as written.
    private static func columns(_ paragraph: String) -> Int {
        paragraph.unicodeScalars.reduce(0) { total, scalar in
            let v = scalar.value
            let wide = (0x1100...0x115F).contains(v) || (0x2E80...0xA4CF).contains(v) || (0xAC00...0xD7A3).contains(v)
                || (0xF900...0xFAFF).contains(v) || (0xFF00...0xFF60).contains(v)
            return total + (wide ? 2 : 1)
        }
    }

    /// The longest text a reader takes in without a break. Longer text goes into short paragraphs
    /// (a line break, or a blank line in a dialog) or a list.
    private static let paragraphColumns = 140

    func test_fr17_no_paragraph_is_longer_than_140_columns() throws {
        for (key, values) in try catalog() {
            for language in ["en", "ko"] {
                for paragraph in (values[language] ?? "").components(separatedBy: "\n") {
                    let width = StringCatalogTests.columns(paragraph)
                    XCTAssertLessThanOrEqual(
                        width, StringCatalogTests.paragraphColumns,
                        "\(key) (\(language)) has a paragraph of \(width) columns; break it up: \(paragraph)")
                }
            }
        }
    }

    func test_fr17_korean_breaks_its_lines_where_english_does() throws {
        for (key, values) in try catalog() {
            let english = (values["en"] ?? "").filter { $0 == "\n" }.count
            let korean = (values["ko"] ?? "").filter { $0 == "\n" }.count
            XCTAssertEqual(korean, english, "\(key) has \(english) line breaks in English but \(korean) in Korean, so they do not say the same things in the same order")
        }
    }

    func test_fr17_pages_use_the_shared_note_and_leave_row_spacing_to_the_form() throws {
        let folder = package.appendingPathComponent("Sources/DsaSettings")
        // The shared views themselves, and the window around the pages, are not pages.
        let notPages: Set<String> = ["Components", "SettingsWindow", "DsaSettingsApp", "MenuBarIcon", "Shell"]
        // Sheets and the log page are not rows of a grouped form, so they set their own margins.
        let ownMargins: Set<String> = ["RecoveryKeySheet", "ImportKeySheet", "LogsPage", "LogRow"]
        let handStyledNote = try NSRegularExpression(pattern: #"\.font\(\.callout\)\s*\.foregroundStyle\(\.secondary\)"#)
        let files = try FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "swift" && !notPages.contains($0.deletingPathExtension().lastPathComponent) }
        XCTAssertGreaterThanOrEqual(files.count, 5)
        for file in files {
            let source = try String(contentsOf: file, encoding: .utf8)
            let matches = handStyledNote.numberOfMatches(in: source, range: NSRange(source.startIndex..., in: source))
            XCTAssertEqual(matches, 0, "\(file.lastPathComponent) styles a note itself instead of using Note")
            // Each view in the file, from its struct line to the next one.
            for view in source.components(separatedBy: "\nstruct ").dropFirst() {
                let name = String(view.prefix { $0.isLetter || $0.isNumber })
                if ownMargins.contains(name) {
                    continue
                }
                XCTAssertFalse(view.contains(".padding("),
                               "\(name) in \(file.lastPathComponent) pads its rows itself; the grouped form spaces them")
            }
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
