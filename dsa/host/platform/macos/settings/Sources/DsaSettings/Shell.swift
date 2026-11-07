import AppKit
import DsaSettingsCore
import UniformTypeIdentifiers

/// The app's texts, from its String Catalog in the user's language.
let appText = BundleText()

/// The text `key` in the user's language.
func T(_ key: String) -> String { appText.get(key) }

/// What the app does on the Mac itself: folders in the Finder, questions, and the few changes
/// that need an administrator or launchd.
@MainActor
enum Shell {
    static var home: URL { FileManager.default.homeDirectoryForCurrentUser }

    /// This user's DSA folder, where the service keeps the user's Android and its socket.
    static var userRoot: URL { home.appendingPathComponent("Library/Application Support/DSA") }

    /// The socket of this user's DSA service, which dsa run as an administrator is pointed at.
    static var socket: URL { userRoot.appendingPathComponent("host.sock") }

    /// The logs of this user's window manager and app windows.
    static var userLogFolder: URL { userRoot.appendingPathComponent("logs") }

    /// The logs of the service and of Android: the folder the service reports, or else the
    /// `logs` folder of the machine's DSA folder.
    static func serviceLogFolder(_ reported: String) -> URL {
        reported.isEmpty
            ? URL(fileURLWithPath: "/Library/Application Support/DSA/logs")
            : URL(fileURLWithPath: reported)
    }

    /// The licences of every component DSA ships, inside DSA.app.
    static var licenseFolder: URL {
        Bundle.main.resourceURL?.appendingPathComponent("licenses")
            ?? URL(fileURLWithPath: "/Applications/DSA.app/Contents/Resources/licenses")
    }

    /// This app's version, as the package stamps it.
    static var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
    }

    /// Whether the app runs as an administrator (root); a desktop app normally does not.
    static var isAdministrator: Bool { getuid() == 0 }

    /// Opens `folder` in the Finder, or says why it cannot.
    static func open(folder: URL) {
        var isFolder: ObjCBool = false
        guard FileManager.default.fileExists(atPath: folder.path, isDirectory: &isFolder), isFolder.boolValue else {
            tell(T("FolderMissingTitle"), Format.fill(T("FolderMissingBody"), [folder.path]))
            return
        }
        NSWorkspace.shared.open(folder)
    }

    /// Shows `message` under `title`, with an OK button.
    static func tell(_ title: String, _ message: String) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.addButton(withTitle: T("OkButton"))
        alert.runModal()
    }

    /// Asks before doing something that cannot be taken back; true when the user chose
    /// `action`.
    static func confirm(_ title: String, _ message: String, _ action: String, destructive: Bool = true) -> Bool {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.alertStyle = destructive ? .critical : .warning
        let button = alert.addButton(withTitle: action)
        button.hasDestructiveAction = destructive
        alert.addButton(withTitle: T("CancelButton"))
        return alert.runModal() == .alertFirstButtonReturn
    }

    /// Quotes `text` for /bin/sh.
    nonisolated static func shellQuote(_ text: String) -> String {
        "'" + text.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    /// Quotes `text` as an AppleScript string.
    nonisolated static func appleScriptQuote(_ text: String) -> String {
        "\"" + text.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"") + "\""
    }

    /// Runs a program and gives its exit code and what it said on standard error.
    nonisolated static func run(_ program: String, _ args: [String]) async -> (Int32, String) {
        await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let process = Process()
                process.executableURL = URL(fileURLWithPath: program)
                process.arguments = args
                let errorPipe = Pipe()
                process.standardError = errorPipe
                process.standardOutput = FileHandle.nullDevice
                do {
                    try process.run()
                } catch {
                    continuation.resume(returning: (-1, error.localizedDescription))
                    return
                }
                let said = errorPipe.fileHandleForReading.readDataToEndOfFile()
                process.waitUntilExit()
                continuation.resume(returning: (process.terminationStatus,
                                                String(decoding: said, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)))
            }
        }
    }

    /// Runs dsa with `args` as an administrator: macOS asks for an administrator's password.
    /// dsa is pointed at this user's service, since it would look in root's home otherwise.
    /// Returns an error to show, or nil.
    static func runDsaElevated(_ args: [String]) async -> String? {
        let dsa = ProcessDsaCli.locate()
        let command = ([dsa, "--socket", socket.path] + args).map(shellQuote).joined(separator: " ")
        let script = "do shell script \(appleScriptQuote(command)) with prompt \(appleScriptQuote(T("Elevation_Prompt"))) with administrator privileges"
        let (code, said) = await run("/usr/bin/osascript", ["-e", script])
        if code == 0 {
            return nil
        }
        // osascript reports a cancelled password prompt as error -128.
        if said.contains("-128") {
            return T("ElevationDeclined")
        }
        return said.isEmpty ? Format.fill(T("ElevatedFailed"), [String(code)]) : said
    }

    /// Restarts this user's window manager through its launchd agent, so it chooses its render
    /// GPU anew. Returns an error to show, or nil.
    static func restartWindowManager() async -> String? {
        let target = "gui/\(getuid())/io.github.thisisthepy.dsa.wm"
        let (code, said) = await run("/bin/launchctl", ["kickstart", "-k", target])
        if code == 0 {
            return nil
        }
        return said.isEmpty ? "launchctl kickstart -k \(target) ended with code \(code)" : said
    }

    /// Asks where to save the diagnostics archive; nil when the user cancels.
    static func askDiagnosticsPath() -> String? {
        let panel = NSSavePanel()
        panel.title = T("Logs_SaveDialogTitle")
        let stamp = DateFormatter()
        stamp.dateFormat = "yyyyMMdd-HHmmss"
        panel.nameFieldStringValue = "dsa-diagnostics-\(stamp.string(from: Date())).zip"
        panel.allowedContentTypes = [.zip]
        return panel.runModal() == .OK ? panel.url?.path : nil
    }

    /// Opens `url` in the default browser.
    static func open(link: String) {
        if let url = URL(string: link) {
            NSWorkspace.shared.open(url)
        }
    }
}
