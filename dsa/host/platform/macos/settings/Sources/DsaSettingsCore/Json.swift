import Foundation

/// One value of what dsa printed as JSON, read the forgiving way the settings apps read it: a
/// field that is missing or of another type reads as empty, zero or false, so a service from
/// before a field existed still shows its page.
public struct Json {
    public let value: Any?

    public init(_ value: Any?) {
        self.value = value
    }

    /// Parses what dsa printed.
    public static func parse(_ text: String) throws -> Json {
        do {
            let value = try JSONSerialization.jsonObject(with: Data(text.utf8), options: [.fragmentsAllowed])
            return Json(value)
        } catch {
            throw DsaError(.unreadable, "dsa printed something other than JSON: \(error.localizedDescription)")
        }
    }

    public var isObject: Bool { value is [String: Any] }
    public var isArray: Bool { value is [Any] }

    public subscript(_ name: String) -> Json {
        Json((value as? [String: Any])?[name])
    }

    public func has(_ name: String) -> Bool {
        (value as? [String: Any])?[name] != nil
    }

    private static func isBoolean(_ number: NSNumber) -> Bool {
        CFGetTypeID(number) == CFBooleanGetTypeID()
    }

    /// The number, when this is a number (and not true or false).
    public var number: NSNumber? {
        guard let number = value as? NSNumber, !Json.isBoolean(number) else {
            return nil
        }
        return number
    }

    public var string: String? { value as? String }

    public var isTrue: Bool {
        guard let number = value as? NSNumber, Json.isBoolean(number) else {
            return false
        }
        return number.boolValue
    }

    public var items: [Json] {
        (value as? [Any])?.map { Json($0) } ?? []
    }

    public func optionalString(_ name: String) -> String? { self[name].string }

    public func string(_ name: String) -> String { self[name].string ?? "" }

    public func bool(_ name: String) -> Bool { self[name].isTrue }

    /// The switch `name`, or `missing` when a service from before it existed does not report it.
    public func bool(_ name: String, missing: Bool) -> Bool {
        has(name) ? self[name].isTrue : missing
    }

    public func uint(_ name: String) -> UInt64 {
        guard let number = self[name].number, number.doubleValue >= 0 else {
            return 0
        }
        return number.uint64Value
    }

    public func optionalUInt(_ name: String) -> UInt64? {
        guard let number = self[name].number, number.doubleValue >= 0 else {
            return nil
        }
        return number.uint64Value
    }

    public func int(_ name: String) -> Int {
        guard let number = self[name].number else {
            return 0
        }
        let double = number.doubleValue
        guard double >= Double(Int32.min), double <= Double(Int32.max), double.rounded() == double else {
            return 0
        }
        return number.intValue
    }

    public func int64(_ name: String) -> Int64 {
        self[name].number?.int64Value ?? 0
    }

    public func strings(_ name: String) -> [String] {
        self[name].items.compactMap(\.string)
    }

    /// A base64 field as bytes; empty when it is missing or not base64.
    public func base64(_ name: String) -> Data {
        Data(base64Encoded: string(name)) ?? Data()
    }
}
