import Foundation

/// The link that opens the settings on the Apps page with one app's row in view, as a task
/// window's App Info opens it: `dsa-settings://apps/com.example.notes`. A link reaches the app
/// whether it is running already (in the menu bar) or has to start, which a command line
/// argument would not.
public enum AppPageLink {
    /// The URL scheme DSA.app opens.
    public static let scheme = "dsa-settings"

    /// The package an Apps page link names, or nil for any other link and for a name that is
    /// no Android package.
    public static func package(of url: URL) -> String? {
        guard url.scheme?.lowercased() == scheme, url.host?.lowercased() == "apps" else {
            return nil
        }
        let package = url.path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        return isPackage(package) ? package : nil
    }

    /// Whether `url` is the link that opens the Updates page, `dsa-settings://updates`, which a
    /// click on DSA's notification about an update opens.
    public static func isUpdates(_ url: URL) -> Bool {
        url.scheme?.lowercased() == scheme && url.host?.lowercased() == "updates"
            && url.path.trimmingCharacters(in: CharacterSet(charactersIn: "/")).isEmpty
    }

    /// Whether `name` is an Android package name: two or more dot-separated parts, each a
    /// letter followed by letters, digits and underscores.
    public static func isPackage(_ name: String) -> Bool {
        name.count <= 255
            && name.range(of: #"^[A-Za-z][A-Za-z0-9_]*(\.[A-Za-z][A-Za-z0-9_]*)+$"#, options: .regularExpression) != nil
    }
}
