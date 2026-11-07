import Foundation

/// Looks up a text in the user's language. The app reads its String Catalog; the tests give
/// the key back.
public protocol TextSource: AnyObject {
    func get(_ key: String) -> String
}

public extension TextSource {
    /// The text `key` with `{0}`, `{1}` and so on filled in from `args`, in order.
    func format(_ key: String, _ args: CustomStringConvertible...) -> String {
        Format.fill(get(key), args.map(\.description))
    }
}

/// The texts of the app's String Catalog, as the bundle compiled them for the user's
/// language.
public final class BundleText: TextSource {
    private let bundle: Bundle

    public init(bundle: Bundle = .main) {
        self.bundle = bundle
    }

    public func get(_ key: String) -> String {
        bundle.localizedString(forKey: key, value: key, table: "Localizable")
    }
}

public enum Format {
    /// Fills `{0}`, `{1}` and so on in `template` with `args`. The catalog keeps the
    /// placeholders the Windows app's texts use, so both apps say the same in both languages.
    public static func fill(_ template: String, _ args: [String]) -> String {
        var text = template
        for (index, arg) in args.enumerated() {
            text = text.replacingOccurrences(of: "{\(index)}", with: arg)
        }
        return text
    }

    private static func decimal(_ value: Double, digits: Int, locale: Locale) -> String {
        let formatter = NumberFormatter()
        formatter.locale = locale
        formatter.numberStyle = .decimal
        formatter.usesGroupingSeparator = false
        formatter.minimumFractionDigits = digits
        formatter.maximumFractionDigits = digits
        return formatter.string(from: NSNumber(value: value)) ?? String(value)
    }

    /// A size in KiB as MB below 1 GB and as GB above, in the user's number format.
    public static func size(_ kb: UInt64, locale: Locale = .current) -> String {
        let mb = Double(kb) / 1024.0
        return mb >= 1024
            ? "\(decimal(mb / 1024.0, digits: 2, locale: locale)) GB"
            : "\(decimal(mb, digits: 1, locale: locale)) MB"
    }

    /// What went wrong, in the user's language, with what dsa said after it.
    public static func problem(_ text: TextSource, _ error: Error) -> String {
        if let dsa = error as? DsaError {
            let key: String
            switch dsa.problem {
            case .cliMissing: key = "Problem_CliMissing"
            case .serviceUnreachable: key = "Problem_ServiceUnreachable"
            case .unreadable: key = "Problem_Unreadable"
            case .refused: key = "Problem_Refused"
            }
            return "\(text.get(key)) (\(dsa.detail))"
        }
        return "\(text.get("Problem_Refused")) (\(error.localizedDescription))"
    }
}
