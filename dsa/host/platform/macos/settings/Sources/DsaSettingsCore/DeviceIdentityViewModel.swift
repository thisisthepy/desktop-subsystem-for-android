import Foundation

/// The rules the service applies to each device identity value, so the page can say what is
/// wrong before anything is sent. The service checks again; what it refuses is shown as an
/// error.
public enum DeviceIdentityRules {
    private static func matches(_ value: String, _ pattern: String) -> Bool {
        value.range(of: pattern, options: .regularExpression) != nil
    }

    private static func printable(_ value: String) -> Bool { matches(value, #"^[\x20-\x7E]{1,64}$"#) }
    static func nameLike(_ value: String) -> Bool { matches(value, #"^[A-Za-z0-9][A-Za-z0-9._-]{0,31}$"#) }
    static func part(_ value: String) -> Bool { matches(value, #"^[A-Za-z0-9._-]{1,64}$"#) }
    static func tags(_ value: String) -> Bool { matches(value, #"^[A-Za-z0-9._,-]{1,64}$"#) }

    /// The text key of what is wrong with `value` for the field `key`, or nil when it is
    /// allowed. Empty is always allowed: it is the default.
    public static func check(_ key: String, _ value: String) -> String? {
        if value.isEmpty {
            return nil
        }
        let ok: Bool
        switch key {
        case "manufacturer", "model":
            ok = printable(value) && value.trimmingCharacters(in: .whitespaces) == value
        case "brand", "device", "name":
            ok = nameLike(value)
        case "characteristics":
            ok = value.count <= 64 && matches(value, #"^[a-z0-9_]+(,[a-z0-9_]+)*$"#)
        case "fingerprint":
            ok = isFingerprint(value)
        case "fingerprint.release", "fingerprint.id", "fingerprint.incremental":
            ok = part(value)
        case "fingerprint.type":
            ok = ["user", "userdebug", "eng"].contains(value)
        case "fingerprint.tags":
            ok = tags(value)
        default:
            ok = false
        }
        return ok ? nil : "Identity_\(key.replacingOccurrences(of: ".", with: "_"))_Invalid"
    }

    /// Whether `value` reads brand/name/device:release/id/incremental:type/tags with allowed
    /// parts.
    public static func isFingerprint(_ value: String) -> Bool {
        if value.count > 256 {
            return false
        }
        let sections = value.split(separator: ":", omittingEmptySubsequences: false).map(String.init)
        guard sections.count == 3 else {
            return false
        }
        let product = sections[0].split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        let build = sections[1].split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        let kind = sections[2].split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        return product.count == 3 && build.count == 3 && kind.count == 2
            && product.allSatisfy(nameLike) && build.allSatisfy(part)
            && ["user", "userdebug", "eng"].contains(kind[0]) && tags(kind[1])
    }
}

/// One value of the device identity on the page: what the user typed, what Android reports
/// when it is left empty, and why the typed value is not allowed, if it is not.
public struct IdentityField: Equatable, Identifiable, Sendable {
    /// The name dsa takes: `dsa settings set device.<key>`.
    public var key: String
    /// What the user typed; empty means the default: the Mac's value or DSA's.
    public var value = ""
    var saved = ""
    /// What Android reports when the box is empty, shown in it as a hint.
    public var placeholder = ""
    /// Where the default came from: `computer` or `dsa`.
    public var origin = "dsa"

    public var id: String { key }
    /// The key in dsa's JSON.
    public var jsonKey: String { key.replacingOccurrences(of: ".", with: "_") }
    public var isEdited: Bool { value != saved }
    public var problemKey: String? { DeviceIdentityRules.check(key, value) }
}

/// A value Android reports that follows the Mac and is not chosen: the model name, the device
/// name and the chip.
public struct ComputerValue: Equatable, Identifiable, Sendable {
    /// The key in dsa's JSON.
    public var key: String
    /// The value; empty when the Mac does not say, and Android keeps its own.
    public var value = ""
    /// Where it came from: `computer` or `dsa`.
    public var origin = "dsa"

    public var id: String { key }
}

/// The Device identity page: the names and build fingerprint Android reports to its apps,
/// which the user may change. Android reads them when it starts, so a saved change applies
/// from its next start; the page offers to restart Android then.
@MainActor
public final class DeviceIdentityViewModel: ObservableObject {
    /// The fields, in the order the page shows them.
    public static let keys = [
        "manufacturer", "brand", "model", "device", "name", "characteristics",
        "fingerprint.release", "fingerprint.id", "fingerprint.incremental", "fingerprint.type",
        "fingerprint.tags", "fingerprint",
    ]

    private let service: DsaService
    private let text: TextSource

    /// The values that follow the Mac, in the order the page shows them.
    public static let computerKeys = ["model_name", "device_name", "soc_manufacturer", "soc_model"]

    @Published public private(set) var fields: [String: IdentityField]
    /// The model name, the device name and the chip.
    @Published public private(set) var computerValues: [ComputerValue] =
        DeviceIdentityViewModel.computerKeys.map { ComputerValue(key: $0) }
    @Published public private(set) var error: String?
    @Published public private(set) var message: String?
    @Published public private(set) var isBusy = false
    @Published private var loaded = false
    @Published private var savedSinceStart = false
    @Published private var state = SessionState.unknown

    public init(service: DsaService, text: TextSource) {
        self.service = service
        self.text = text
        fields = Dictionary(uniqueKeysWithValues: DeviceIdentityViewModel.keys.map { ($0, IdentityField(key: $0)) })
    }

    /// Manufacturer, brand, model, device, product name and form factor.
    public var names: [IdentityField] { DeviceIdentityViewModel.keys.prefix(6).compactMap { fields[$0] } }
    /// The fingerprint's release, build ID, incremental version, type and tags.
    public var fingerprintParts: [IdentityField] { DeviceIdentityViewModel.keys.dropFirst(6).prefix(5).compactMap { fields[$0] } }
    /// The whole fingerprint string, which wins over its parts when set.
    public var wholeFingerprint: IdentityField { fields["fingerprint"]! }

    public func field(_ key: String) -> IdentityField { fields[key]! }

    public func label(_ field: IdentityField) -> String { text.get("Identity_\(field.jsonKey)") }
    public func detail(_ field: IdentityField) -> String { text.get("Identity_\(field.jsonKey)_Description") }
    public func problem(_ field: IdentityField) -> String { field.problemKey.map { text.get($0) } ?? "" }

    /// Where a default came from, for people: this Mac, or DSA's own value.
    public func originText(_ origin: String) -> String {
        text.get(origin == "computer" ? "Identity_FromComputer" : "Identity_FromDsa")
    }

    public func computerValue(_ key: String) -> ComputerValue {
        computerValues.first { $0.key == key } ?? ComputerValue(key: key)
    }

    public func label(_ value: ComputerValue) -> String { text.get("Identity_\(value.key)") }
    public func detail(_ value: ComputerValue) -> String { text.get("Identity_\(value.key)_Description") }
    /// The value as the page shows it: a value the Mac does not say says so.
    public func shown(_ value: ComputerValue) -> String {
        value.value.isEmpty ? text.get("Identity_NotSaid") : value.value
    }

    /// Changes what is typed in the box `key`.
    public func edit(_ key: String, _ value: String) {
        fields[key]?.value = value
    }

    public var hasEdits: Bool { fields.values.contains(where: \.isEdited) }
    public var hasProblems: Bool { fields.values.contains { $0.problemKey != nil } }
    public var canSave: Bool { loaded && !isBusy && hasEdits && !hasProblems }
    public var canReset: Bool { loaded && !isBusy }
    public var isRunning: Bool { [.starting, .active, .dozing, .frozen, .stalled].contains(state) }
    /// Whether a saved change waits for Android to start again.
    public var needsRestart: Bool { savedSinceStart && isRunning }
    public var canRestart: Bool { needsRestart && !isBusy }

    /// The fingerprint Android will report from its next start, with what is typed now.
    public var fingerprintPreview: String {
        func now(_ key: String) -> String {
            let field = fields[key]!
            return field.value.isEmpty ? field.placeholder : field.value
        }
        let whole = fields["fingerprint"]!
        if !whole.value.isEmpty {
            return whole.value
        }
        let parts = ["fingerprint.release", "fingerprint.id", "fingerprint.incremental"].map(now)
        if parts.contains(where: \.isEmpty) {
            return whole.placeholder
        }
        return "\(now("brand"))/\(now("name"))/\(now("device")):\(parts[0])/\(parts[1])/\(parts[2]):\(now("fingerprint.type"))/\(now("fingerprint.tags"))"
    }

    private func show(_ report: Json) {
        // A service from before the Mac's identity names no source: every default is DSA's.
        func origin(_ jsonKey: String) -> String {
            report["sources"].string(jsonKey) == "computer" ? "computer" : "dsa"
        }
        for key in DeviceIdentityViewModel.keys {
            let jsonKey = key.replacingOccurrences(of: ".", with: "_")
            let chosen = report["chosen"].string(jsonKey)
            fields[key] = IdentityField(key: key, value: chosen, saved: chosen, placeholder: report["effective"].string(jsonKey),
                                        origin: origin(jsonKey))
        }
        computerValues = DeviceIdentityViewModel.computerKeys.map {
            ComputerValue(key: $0, value: report["effective"].string($0), origin: origin($0))
        }
        loaded = true
    }

    private func run(_ work: () async throws -> Void) async {
        isBusy = true
        error = nil
        do {
            try await work()
        } catch {
            self.error = Format.problem(text, error)
        }
        isBusy = false
    }

    private func readState() async {
        state = (try? await service.status().state) ?? .unknown
    }

    public func load() async {
        await run {
            message = nil
            show(try await service.runJson(["settings", "device"]))
            await readState()
        }
    }

    /// Sends each value the user changed: a typed one is set, an emptied one goes back to the
    /// image's own.
    public func save() async {
        await run {
            if hasProblems {
                return
            }
            var last: Json?
            for key in DeviceIdentityViewModel.keys {
                guard let field = fields[key], field.isEdited else {
                    continue
                }
                if field.value.isEmpty {
                    last = try await service.runJson(["settings", "reset", "device.\(key)"])
                } else {
                    last = try await service.runJson(["settings", "set", "device.\(key)", field.value])
                }
            }
            if let report = last {
                show(report)
                await readState()
                savedSinceStart = true
                message = text.get(isRunning ? "Identity_SavedRestart" : "Identity_Saved")
            }
        }
    }

    /// Puts every value back to the image's own.
    public func reset() async {
        await run {
            show(try await service.runJson(["settings", "reset", "device"]))
            await readState()
            savedSinceStart = true
            message = text.get(isRunning ? "Identity_ResetRestart" : "Identity_Reset")
        }
    }

    /// Stops Android and starts it again, so it reads the saved identity.
    public func restart() async {
        await run {
            _ = try await service.stop()
            state = try await service.start().state
            savedSinceStart = false
            message = text.get("Identity_Restarted")
        }
    }
}
